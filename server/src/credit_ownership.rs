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
    stable["identity"] = pick(
        &row["credentials"],
        &["plan_type", "chatgpt_account_id", "email"],
    );
    stable["extra"] = row["extra"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| {
            ![
                "codex_5h_",
                "codex_7d_",
                "codex_usage_",
                "codex_reset_credit_",
                "codex_credits_",
            ]
            .iter()
            .any(|p| key.starts_with(p))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();
    config_revision(&stable)
}
pub fn parameter_guard(row: &Value) -> String {
    config_revision(&json!([
        signature(row),
        row["credentials"],
        pick(
            row,
            &[
                "status",
                "schedulable",
                "load_factor",
                "proxy_id",
                "rate_limited_at",
                "rate_limit_reset_at",
                "overload_until",
                "temp_unschedulable_until",
                "temp_unschedulable_reason"
            ]
        )
    ]))
}
pub fn generation(state: &Value, id: i64) -> i64 {
    integer(&state["scheduler"][id.to_string()]["manual_control"]["generation"]).unwrap_or(0)
}
pub fn owned(row: &Value, task: &Value, state: &Value, same_version: bool) -> bool {
    truthy(task)
        && task["owns_pause"] == true
        && row["schedulable"] == false
        && task["signature"] == signature(row)
        && (task["control_generation"].is_null()
            || integer(&task["control_generation"])
                == Some(generation(state, integer(&row["id"]).unwrap_or(0))))
        && (!same_version || row["updated_at"] == task["owned_version"])
}
pub fn save(store: &OAuthState, id: i64, task: &Value, changes: Value) -> Result<Value> {
    let mut updated = task.clone();
    updated
        .as_object_mut()
        .ok_or_else(Error::internal)?
        .extend(changes.as_object().cloned().ok_or_else(Error::internal)?);
    updated["revision"] = (integer(&task["revision"]).unwrap_or(0) + 1).into();
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
        let checked = [&current["policy_checked_at"], &updated["policy_checked_at"]]
            .into_iter()
            .filter_map(crate::compat::parse_datetime)
            .max();
        if let Some(at) = checked {
            updated["policy_checked_at"] = crate::compat::datetime(at).into();
        }
        meta["auto_reset_credit"] = updated.clone();
        Ok(updated.clone())
    })
}
pub fn reconcile(store: &OAuthState, row: &Value) -> Result<Value> {
    let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
    let state = store.snapshot()?;
    let task = state["scheduler"][id.to_string()]["auto_reset_credit"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let task = Value::Object(task);
    let pending = &task["parameter_edit"];
    if !pending.is_object()
        || task["owns_pause"] != true
        || integer(&pending["generation"]) != Some(generation(&state, id))
        || row["schedulable"] != false
    {
        return Ok(task);
    }
    if pending["guard"] == parameter_guard(row) {
        save(
            store,
            id,
            &task,
            json!({"signature":signature(row),"owned_version":row["updated_at"],"parameter_edit":null}),
        )
    } else if owned(row, &task, &state, true) {
        save(store, id, &task, json!({"parameter_edit":null}))
    } else {
        Ok(task)
    }
}
pub fn prepare(store: &OAuthState, row: &Value, field: &str, value: &Value) -> Result<()> {
    if !matches!(field, "priority" | "concurrency" | "load_factor") {
        return Err(Error::invalid());
    }
    let task = reconcile(store, row)?;
    if task["owns_pause"] != true {
        return Ok(());
    }
    let state = store.snapshot()?;
    if !owned(row, &task, &state, true) {
        return Err(Error::new(409, "用卡暂停归属待核对，未修改参数"));
    }
    let id = integer(&row["id"]).ok_or_else(Error::invalid)?;
    let mut desired = row.clone();
    if field == "priority" {
        for key in ["account_priority", "priority"] {
            if desired.get(key).is_some() {
                desired[key] = value.clone();
            }
        }
    } else {
        desired[field] = value.clone();
    }
    save(
        store,
        id,
        &task,
        json!({"parameter_edit":{"field":field,"target":value,"guard":parameter_guard(&desired),"generation":generation(&state,id)}}),
    )?;
    Ok(())
}
pub fn cancel(store: &OAuthState, id: i64) -> Result<()> {
    let state = store.snapshot()?;
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
