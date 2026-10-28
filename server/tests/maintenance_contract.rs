use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use sub2ops_server::maintenance::{migrate_desktop, migrate_oauth};

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn oauth_fixture(root: &Path) -> BTreeMap<String, String> {
    fs::write(root.join("old.json"),json!({"bot_token":"fixture-retired-bot-token","oauth_daily_test_enabled":false,"oauth_daily_test_time":"05:15","oauth_recovery_monitor_enabled":false,"oauth_recovery_test_model_id":"fixture-preserved-model"}).to_string()).unwrap();
    fs::write(root.join("pairing.json"), r#"{"paired_user_ids":[1]}"#).unwrap();
    fs::write(root.join("state.json"),json!({"admin_token":"fixture-preserved-admin","pending_events":{"event":{"id":"keep"}},"daily_test":{"time":"05:15"},"oauth_results":{"1":{"summary":{"telegram_windows":[1],"ui_windows":[2]}}}}).to_string()).unwrap();
    fs::write(root.join(".env"),"DATABASE_URL=fixture-preserved\nTELEGRAM_BOT_TOKEN=fixture-retired-bot-token\nexport TELEGRAM_OAUTH_DAILY_TEST_TIME=05:00\nBARK_DEVICE_KEY=fixture-preserved-bark\n").unwrap();
    BTreeMap::from([
        (
            "TELEGRAM_CONFIG_PATH".into(),
            root.join("old.json").display().to_string(),
        ),
        (
            "TELEGRAM_STATE_PATH".into(),
            root.join("pairing.json").display().to_string(),
        ),
        (
            "OAUTH_CONFIG_PATH".into(),
            root.join("oauth.json").display().to_string(),
        ),
        (
            "USAGE_QUERY_STATE_PATH".into(),
            root.join("state.json").display().to_string(),
        ),
        (
            "TELEGRAM_OAUTH_USAGE_REFRESH_CONCURRENCY".into(),
            "7".into(),
        ),
    ])
}
#[test]
fn oauth_offline_migration_preserves_effective_configuration_state_and_rerun_bytes() {
    let root = tempfile::tempdir().unwrap();
    let env = oauth_fixture(root.path());
    let result = migrate_oauth(&env, &root.path().join(".env")).unwrap();
    assert_eq!(result["legacy_removed"], true);
    let config = read(&root.path().join("oauth.json"));
    assert_eq!(config["oauth_daily_test_time"], "05:15");
    assert_eq!(config["oauth_daily_test_enabled"], false);
    assert_eq!(config["oauth_recovery_monitor_enabled"], false);
    assert_eq!(config["oauth_usage_refresh_concurrency"], 7);
    assert_eq!(
        config["oauth_recovery_test_model_id"],
        "fixture-preserved-model"
    );
    let state = read(&root.path().join("state.json"));
    assert_eq!(state["admin_token"], "fixture-preserved-admin");
    assert_eq!(state["pending_events"], json!({"event":{"id":"keep"}}));
    assert_eq!(
        state["oauth_results"]["1"]["summary"],
        json!({"ui_windows":[2]})
    );
    let neutral = fs::read_to_string(root.path().join(".env")).unwrap();
    assert!(neutral.contains("BARK_DEVICE_KEY=fixture-preserved-bark"));
    assert!(!neutral.contains("TELEGRAM_"));
    assert!(!neutral.contains("fixture-retired-bot-token"));
    assert!(!config.to_string().contains("fixture-retired-bot-token"));
    for name in ["oauth.json", "state.json", ".env"] {
        assert_eq!(
            fs::metadata(root.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!root.path().join("old.json").exists());
    assert!(!root.path().join("pairing.json").exists());
    let before = fs::read(root.path().join("oauth.json")).unwrap();
    migrate_oauth(&env, &root.path().join(".env")).unwrap();
    assert_eq!(fs::read(root.path().join("oauth.json")).unwrap(), before);
}
#[test]
fn oauth_offline_migration_current_values_win_and_invalid_sources_never_delete_history() {
    let root = tempfile::tempdir().unwrap();
    let env = oauth_fixture(root.path());
    fs::write(
        root.path().join("oauth.json"),
        r#"{"oauth_daily_test_time":"07:31","oauth_recovery_connection_account_ids":[7]}"#,
    )
    .unwrap();
    migrate_oauth(&env, &root.path().join(".env")).unwrap();
    assert_eq!(
        read(&root.path().join("oauth.json"))["oauth_daily_test_time"],
        "07:31"
    );
    assert_eq!(
        read(&root.path().join("oauth.json"))["oauth_recovery_connection_account_ids"],
        json!([7])
    );
    let second = tempfile::tempdir().unwrap();
    let env = oauth_fixture(second.path());
    fs::write(
        second.path().join("old.json"),
        r#"{"oauth_daily_test_time":"invalid"}"#,
    )
    .unwrap();
    let before = fs::read(second.path().join(".env")).unwrap();
    assert!(migrate_oauth(&env, &second.path().join(".env")).is_err());
    assert_eq!(fs::read(second.path().join(".env")).unwrap(), before);
    assert!(second.path().join("old.json").exists());
    assert!(second.path().join("pairing.json").exists());
    assert!(!second.path().join("oauth.json").exists());
}
#[test]
fn oauth_offline_migration_failed_atomic_write_keeps_sources_and_cleans_temporary_files() {
    let root = tempfile::tempdir().unwrap();
    let env = oauth_fixture(root.path());
    let before = fs::read(root.path().join(".env")).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let failed = migrate_oauth(&env, &root.path().join(".env"));
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(failed.is_err());
    assert!(root.path().join("old.json").exists());
    assert!(root.path().join("pairing.json").exists());
    assert!(!root.path().join("oauth.json").exists());
    assert_eq!(fs::read(root.path().join(".env")).unwrap(), before);
    assert!(fs::read_dir(root.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".sub2ops-migration-")));
}
fn desktop_fixture(root: &Path) -> BTreeMap<String, String> {
    fs::write(root.join("sso.json"),json!({"base_url":"https://public.example.invalid/","verify_base_url":"http://127.0.0.1:8080/","enabled":false}).to_string()).unwrap();
    fs::write(
        root.join("sessions.json"),
        r#"{"secret":"fixture-retired-session"}"#,
    )
    .unwrap();
    fs::write(root.join(".env"),"DATABASE_URL=fixture-preserved\nOPS_BASIC_USER=fixture-retired\nOPS_BASIC_PASSWORD=fixture-retired\nOPS_SESSION_SECRET=fixture-retired\nSUB2API_SSO_ENABLED=true\nOAUTH_DAILY_TEST_TIME=05:15\nBARK_DEVICE_KEY=fixture-preserved-bark\n").unwrap();
    BTreeMap::from([
        (
            "OPS_SSO_CONFIG_PATH".into(),
            root.join("sso.json").display().to_string(),
        ),
        (
            "SUB2API_CONFIG_PATH".into(),
            root.join("connection.json").display().to_string(),
        ),
        (
            "OPS_SESSION_STORE_PATH".into(),
            root.join("sessions.json").display().to_string(),
        ),
        (
            "SUB2API_BASE_URL".into(),
            "https://ignored.example.invalid".into(),
        ),
    ])
}
#[tokio::test]
async fn desktop_offline_migration_preserves_effective_urls_secrets_and_only_expected_files() {
    let root = tempfile::tempdir().unwrap();
    let env = desktop_fixture(root.path());
    let result = migrate_desktop(&env, &root.path().join(".env"), false)
        .await
        .unwrap();
    assert_eq!(result["connection_migrated"], true);
    assert_eq!(
        read(&root.path().join("connection.json")),
        json!({"base_url":"https://public.example.invalid","verify_base_url":"http://127.0.0.1:8080"})
    );
    assert_eq!(fs::read_to_string(root.path().join(".env")).unwrap(),"DATABASE_URL=fixture-preserved\nOAUTH_DAILY_TEST_TIME=05:15\nBARK_DEVICE_KEY=fixture-preserved-bark\n");
    for name in ["connection.json", ".env", "group-model-config.json"] {
        assert_eq!(
            fs::metadata(root.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        read(&root.path().join("group-model-config.json")),
        json!({"schema":1,"groups":{}})
    );
    let files: BTreeSet<PathBuf> = fs::read_dir(root.path())
        .unwrap()
        .map(|v| PathBuf::from(v.unwrap().file_name()))
        .collect();
    assert_eq!(
        files,
        BTreeSet::from([
            PathBuf::from("connection.json"),
            PathBuf::from(".env"),
            PathBuf::from("group-model-config.json")
        ])
    );
    assert_eq!(
        migrate_desktop(&env, &root.path().join(".env"), false)
            .await
            .unwrap()["retired_bytes"],
        0
    );
}
#[tokio::test]
async fn desktop_offline_migration_invalid_overlapping_or_symlink_sources_never_delete_state() {
    let root = tempfile::tempdir().unwrap();
    let mut env = desktop_fixture(root.path());
    fs::write(root.path().join("sso.json"), r#"{"base_url":"invalid"}"#).unwrap();
    let before = fs::read(root.path().join(".env")).unwrap();
    let session = fs::read(root.path().join("sessions.json")).unwrap();
    assert!(migrate_desktop(&env, &root.path().join(".env"), false)
        .await
        .is_err());
    assert_eq!(fs::read(root.path().join(".env")).unwrap(), before);
    assert_eq!(
        fs::read(root.path().join("sessions.json")).unwrap(),
        session
    );
    env.insert(
        "SUB2API_CONFIG_PATH".into(),
        root.path().join("sso.json").display().to_string(),
    );
    assert!(migrate_desktop(&env, &root.path().join(".env"), false)
        .await
        .is_err());
    env.insert(
        "SUB2API_CONFIG_PATH".into(),
        root.path().join("connection.json").display().to_string(),
    );
    std::os::unix::fs::symlink(
        root.path().join("sessions.json"),
        root.path().join("linked.json"),
    )
    .unwrap();
    env.insert(
        "OPS_SESSION_STORE_PATH".into(),
        root.path().join("linked.json").display().to_string(),
    );
    assert!(migrate_desktop(&env, &root.path().join(".env"), false)
        .await
        .is_err());
    assert_eq!(
        fs::read(root.path().join("sessions.json")).unwrap(),
        session
    );
    assert!(!root.path().join("connection.json").exists());
    let overlap = tempfile::tempdir().unwrap();
    let mut env = desktop_fixture(overlap.path());
    let source_before = fs::read(overlap.path().join("sso.json")).unwrap();
    let env_before = fs::read(overlap.path().join(".env")).unwrap();
    env.insert(
        "GROUP_MODEL_CONFIG_PATH".into(),
        env["SUB2API_CONFIG_PATH"].clone(),
    );
    assert!(migrate_desktop(&env, &overlap.path().join(".env"), false)
        .await
        .is_err());
    assert_eq!(
        fs::read(overlap.path().join("sso.json")).unwrap(),
        source_before
    );
    assert_eq!(fs::read(overlap.path().join(".env")).unwrap(), env_before);
    assert!(!overlap.path().join("connection.json").exists());
}
#[tokio::test]
async fn desktop_offline_migration_removes_only_retired_menu_on_matching_host_and_path() {
    let root = tempfile::tempdir().unwrap();
    let mut env = desktop_fixture(root.path());
    let database = std::env::var("DATABASE_URL").unwrap();
    env.insert("DATABASE_URL".into(), database.clone());
    let pool = sqlx::PgPool::connect(&database).await.unwrap();
    // The shared fixture keeps a minimal settings table; production has the
    // timestamp column used by the legacy maintenance query.
    sqlx::query("ALTER TABLE settings ADD COLUMN IF NOT EXISTS updated_at timestamptz")
        .execute(&pool)
        .await
        .unwrap();
    let prior: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key='custom_menu_items'")
            .fetch_optional(&pool)
            .await
            .unwrap();
    let items = json!([{"id":1,"url":"https://public.example.invalid/sub2ops/sso/start"},{"id":2,"url":"https://elsewhere.example.invalid/sub2ops/sso/start"},{"id":3,"url":"https://public.example.invalid/other"}]);
    sqlx::query("INSERT INTO settings(key,value) VALUES('custom_menu_items',$1) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(items.to_string()).execute(&pool).await.unwrap();
    let result = migrate_desktop(&env, &root.path().join(".env"), true)
        .await
        .unwrap();
    assert_eq!(result["menu_items_removed"], 1);
    let raw: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key='custom_menu_items'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let retained: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(retained, json!([items[1], items[2]]));
    match prior {
        Some(value) => {
            sqlx::query("UPDATE settings SET value=$1 WHERE key='custom_menu_items'")
                .bind(value)
                .execute(&pool)
                .await
                .unwrap();
        }
        None => {
            sqlx::query("DELETE FROM settings WHERE key='custom_menu_items'")
                .execute(&pool)
                .await
                .unwrap();
        }
    }
    pool.close().await;
}

#[test]
fn migration_preserves_current_retry_and_detection_settings() {
    let root = tempfile::tempdir().unwrap();
    let env = oauth_fixture(root.path());
    let current = json!({"connection_test_retry_interval_seconds":17,"connection_test_max_retries":0,"model_detection_platforms":{"openai":{"engine":"lm_detector","sample_count":3},"anthropic":{"engine":"lm_detector","sample_count":1},"grok":{"engine":"lm_detector","sample_count":1},"deepseek":{"engine":"lm_detector","sample_count":1}},"oauth_early_reset_enabled":true,"oauth_early_reset_interval_seconds":90,"oauth_recovery_seen_account_ids":[7]});
    fs::write(root.path().join("oauth.json"), current.to_string()).unwrap();
    migrate_oauth(&env, &root.path().join(".env")).unwrap();
    let migrated = read(&root.path().join("oauth.json"));
    for (key, value) in current.as_object().unwrap() {
        assert_eq!(&migrated[key], value, "{key}");
    }
}
