use crate::{
    compat::{config_revision, integer, pick, string, truthy},
    db::Database,
    error::{Error, Result},
    oauth_state::{validate_credit, OAuthState},
};
use serde_json::{json, Value};

pub const ACCOUNT_SELECT: &str = "SELECT id,name,platform,type,credentials,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,extra,status,schedulable,priority AS account_priority,concurrency,load_factor,proxy_id,temp_unschedulable_until,temp_unschedulable_reason,rate_limited_at,rate_limit_reset_at,overload_until,error_message,expires_at,auto_pause_on_expired,updated_at,(SELECT created_at FROM usage_logs WHERE account_id=accounts.id ORDER BY created_at DESC,id DESC LIMIT 1) AS last_success_at FROM accounts WHERE deleted_at IS NULL";
pub async fn account(db: &Database, id: i64) -> Result<Value> {
    db.one(&format!("{ACCOUNT_SELECT} AND id=$1"), &[id.into()])
        .await?
        .ok_or_else(|| Error::new(404, "账号不存在"))
}
pub fn signature(row: &Value) -> String {
    let mut stable = pick(
        row,
        &[
            "id",
            "name",
            "platform",
            "type",
            "parent_account_id",
            "account_priority",
            "priority",
            "concurrency",
            "expires_at",
            "auto_pause_on_expired",
        ],
    );
    stable["account_priority"] = priority(row).clone();
    stable["priority"] = Value::Null;
    stable["identity"] = identity(row);
    stable["extra"] = row["extra"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !quota_extra(key))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();
    config_revision(&stable)
}
fn quota_extra(key: &str) -> bool {
    [
        "codex_5h_",
        "codex_7d_",
        "codex_usage_",
        "codex_reset_credit_",
        "codex_credits_",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}
fn priority(row: &Value) -> &Value {
    row.get("account_priority").unwrap_or(&row["priority"])
}
pub fn identity(row: &Value) -> Value {
    row.get("account_identity").cloned().unwrap_or_else(|| {
        pick(
            &row["credentials"],
            &["plan_type", "chatgpt_account_id", "email"],
        )
    })
}
pub fn generation(state: &Value, id: i64) -> i64 {
    integer(&state["scheduler"][id.to_string()]["manual_control"]["generation"]).unwrap_or(0)
}
pub fn owned(row: &Value, task: &Value, state: &Value) -> bool {
    truthy(task)
        && task["owns_pause"] == true
        && row["schedulable"] == false
        && task["signature"] == signature(row)
        && (task["control_generation"].is_null()
            || integer(&task["control_generation"])
                == Some(generation(state, integer(&row["id"]).unwrap_or(0))))
}
pub fn save(store: &OAuthState, id: i64, task: &Value, changes: Value) -> Result<Value> {
    let mut updated = task.clone();
    updated
        .as_object_mut()
        .ok_or_else(Error::internal)?
        .extend(changes.as_object().cloned().ok_or_else(Error::internal)?);
    updated["revision"] = (integer(&task["revision"]).unwrap_or(0) + 1).into();
    crate::oauth_state::retire_credit_fields(&mut updated);
    validate_credit(&updated)?;
    store.transaction(|state| {
        let meta = &mut state["scheduler"][id.to_string()];
        if !meta.is_object() {
            *meta = json!({});
        }
        let current = &meta["auto_reset_credit"];
        if !current["episode"].is_null() && current["episode"] != task["episode"]
            || integer(&current["revision"]).unwrap_or(0) != integer(&task["revision"]).unwrap_or(0)
        {
            return Err(Error::new(409, "自动用卡任务已变化"));
        }
        if changes.get("model_test_job_id").is_none() && truthy(&current["model_test_job_id"]) {
            updated["model_test_job_id"] = current["model_test_job_id"].clone();
        }
        meta["auto_reset_credit"] = updated.clone();
        Ok(updated.clone())
    })
}
pub fn parameter_change(store: &OAuthState, row: &Value) -> Result<Option<Value>> {
    let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
    let state = store.shared_snapshot()?;
    let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
    if task["owns_pause"] != true {
        return Ok(None);
    }
    if !owned(row, task, &state) {
        return Err(Error::new(409, "用卡暂停归属待核对，未修改参数"));
    }
    Ok(Some(task.clone()))
}

pub fn finish_parameter_change(store: &OAuthState, row: &Value, task: &Value) -> Result<()> {
    let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
    let state = store.shared_snapshot()?;
    if row["schedulable"] != false
        || task["control_generation"]
            .as_i64()
            .is_some_and(|expected| expected != generation(&state, id))
    {
        return Err(Error::new(409, "改参后的用卡暂停归属已变化，请重新核对"));
    }
    save(store, id, task, json!({"signature":signature(row)}))?;
    Ok(())
}
pub fn cancel(store: &OAuthState, id: i64) -> Result<()> {
    let state = store.shared_snapshot()?;
    let task = &state["scheduler"][id.to_string()]["auto_reset_credit"];
    if truthy(task) && string(&task["stage"]) != "recovered" {
        save(
            store,
            id,
            task,
            json!({"stage":"manual","owns_pause":false,"next_at":null,"error_code":"","manual_at":crate::compat::now()}),
        )?;
    }
    Ok(())
}
