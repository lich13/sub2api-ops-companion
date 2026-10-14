//! Retire obsolete work by readback; migration never replays account writes.
use crate::{
    compat::{integer, now, string},
    error::{Error, Result},
    runtime::Runtime,
    store::Store,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs};

fn items(data: &Value, profiles: bool) -> Vec<Value> {
    if profiles {
        data["jobs"]
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(_, job)| job["items"].as_array().into_iter().flatten())
            .cloned()
            .collect()
    } else {
        data["profile_intents"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, item)| item.clone())
            .collect()
    }
}
fn retired(item: &mut Value, rows: &BTreeMap<i64, Value>, message: &str) {
    if !matches!(string(&item["status"]), "queued" | "writing") {
        return;
    }
    let matches = item["status"] == "writing"
        && item["target"].is_object()
        && integer(&item["account_id"])
            .and_then(|id| rows.get(&id))
            .is_some_and(|row| {
                row["model_mapping"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    == item["target"].as_object().cloned().unwrap_or_default()
            });
    item["status"] = if matches { "applied" } else { "retired" }.into();
    item["error"] = if matches { "" } else { message }.into();
}

impl Runtime {
    pub(crate) fn legacy_profiles(&self) -> Option<Store> {
        let path = self.settings.data_dir().join("account-model-profiles.json");
        (path.exists() || path.with_extension("lock").exists())
            .then(|| Store::policy(path, json!({}), self.settings.read_only))
    }

    pub async fn retire_legacy_templates(&self) -> Result<()> {
        self.writable()?;
        let profiles = self.legacy_profiles();
        let old = profiles
            .as_ref()
            .map(Store::read)
            .transpose()?
            .unwrap_or(json!({}));
        let capacity = self.detection.capacity.store.snapshot()?;
        let mut rows = BTreeMap::new();
        for item in items(&old, true)
            .into_iter()
            .chain(items(&capacity, false))
            .filter(|item| item["status"] == "writing")
        {
            if let Some(id) = integer(&item["account_id"]) {
                if let std::collections::btree_map::Entry::Vacant(entry) = rows.entry(id) {
                    // A database failure is not evidence that an account changed.
                    match self.actions.account(id, None).await {
                        Ok(row) => {
                            entry.insert(row);
                        }
                        Err(error) if error.status.as_u16() == 404 => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if let Some(profiles) = profiles {
            profiles.transaction(|data| {
                for job in data
                    .get_mut("jobs")
                    .and_then(Value::as_object_mut)
                    .into_iter()
                    .flat_map(|jobs| jobs.values_mut())
                {
                    for item in job["items"].as_array_mut().into_iter().flatten() {
                        retired(item, &rows, "旧模板任务已退休，未重放");
                    }
                }
                Ok(())
            })?;
        }
        self.detection.capacity.store.transaction(|data| {
            for item in data
                .get_mut("profile_intents")
                .and_then(Value::as_object_mut)
                .into_iter()
                .flat_map(|values| values.values_mut())
            {
                retired(item, &rows, "旧模板联动已退休，未重放");
            }
            Ok(())
        })
    }

    pub async fn migrate_legacy_recovery(&self) -> Result<()> {
        self.writable()?;
        self.monitor.store.transaction(|_| Ok(()))?;
        if self.monitor.state()?["settings"]["legacy_recovery_cleanup_completed"] == true {
            return Ok(());
        }
        let deleted = self
            .db
            .execute(crate::sql::LEGACY_LEGACY_RECOVERY_PLAN_CLEANUP_SQL, &[])
            .await?;
        let path = self.settings.data_dir().join("guard-state.json");
        match fs::remove_file(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    fs::File::open(parent)?.sync_all()?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::new(503, "旧恢复状态无法退休")),
        }
        self.monitor.store.transaction(|state| {
            state["settings"]["legacy_recovery_cleanup_completed"] = true.into();
            state["settings"]["legacy_recovery_cleanup_completed_at"] = now().into();
            Ok(())
        })?;
        crate::audit::write(
            &self.settings.audit_path,
            "oauth_legacy_recovery_cleanup",
            json!({"success":true,"skipped":false,"deleted_count":deleted}),
            false,
        );
        Ok(())
    }
}
