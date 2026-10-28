use crate::{
    account_templates,
    compat::{digest, integer, now, string, truthy},
    db::Param,
    error::{Error, Result},
    operations::{id, ids, native, native_confirmed, native_reached, Operations},
    queries::settings_capabilities,
    security::safe_error,
    upstream::read_limited,
};
use futures_util::{future::BoxFuture, FutureExt};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub fn result_ids(
    data: &Value,
    expected: &BTreeSet<i64>,
    action: &str,
) -> Option<(BTreeSet<i64>, BTreeSet<i64>)> {
    let success = data["success_ids"].as_array()?;
    let failed = data["failed_ids"].as_array()?;
    let all: Vec<_> = success
        .iter()
        .chain(failed)
        .map(|v| v.as_i64())
        .collect::<Option<_>>()?;
    if all.iter().any(|id| *id < 1)
        || all.len() != expected.len()
        || all.iter().copied().collect::<BTreeSet<_>>() != *expected
        || data["success"].as_u64() != Some(success.len() as u64)
        || data["failed"].as_u64() != Some(failed.len() as u64)
        || data
            .get("total")
            .is_some_and(|v| v.as_u64() != Some(expected.len() as u64))
    {
        return None;
    }
    let success = ids(&data["success_ids"]);
    let failed = ids(&data["failed_ids"]);
    let mut seen = BTreeSet::new();
    for entry in data[if action == "delete" {
        "errors"
    } else {
        "results"
    }]
    .as_array()?
    {
        let aid = entry["account_id"].as_i64()?;
        if !entry.is_object()
            || !expected.contains(&aid)
            || !seen.insert(aid)
            || action != "delete" && entry["success"].as_bool() != Some(success.contains(&aid))
        {
            return None;
        }
    }
    if seen
        != if action == "delete" {
            failed.clone()
        } else {
            expected.clone()
        }
    {
        return None;
    }
    Some((success, failed))
}
pub fn unsupported(status: u16, bytes: &[u8]) -> bool {
    if !matches!(status, 404 | 405) {
        return false;
    }
    if matches!(
        String::from_utf8_lossy(bytes)
            .trim()
            .to_lowercase()
            .as_str(),
        "404 page not found" | "method not allowed" | "405 method not allowed"
    ) {
        return true;
    }
    serde_json::from_slice::<Value>(bytes)
        .is_ok_and(|v| matches!(string(&v["detail"]), "Not Found" | "Method Not Allowed"))
}
impl Operations {
    pub async fn children(&self, ids: &[i64]) -> Result<BTreeMap<i64, Vec<i64>>> {
        let rows=self.actions.queries.db.all("SELECT a.id,nullif(to_jsonb(a)->>'parent_account_id','')::bigint AS parent_account_id FROM accounts a WHERE a.deleted_at IS NULL AND nullif(to_jsonb(a)->>'parent_account_id','')::bigint=ANY($1)",&[Param::Ints(ids.to_vec())]).await?;
        let mut result: BTreeMap<_, Vec<_>> = ids.iter().map(|id| (*id, vec![])).collect();
        for row in rows {
            if let Some(values) =
                integer(&row["parent_account_id"]).and_then(|id| result.get_mut(&id))
            {
                if let Some(id) = integer(&row["id"]) {
                    values.push(id);
                }
            }
        }
        for values in result.values_mut() {
            values.sort_unstable();
        }
        Ok(result)
    }
    async fn native_rows(&self, jobs: &[Value]) -> Result<BTreeMap<i64, Value>> {
        Ok(self
            .actions
            .accounts(
                &jobs
                    .iter()
                    .filter_map(|j| integer(&j["account_id"]))
                    .collect::<Vec<_>>(),
            )
            .await?
            .into_iter()
            .filter_map(|r| integer(&r["id"]).map(|id| (id, r)))
            .collect())
    }
    async fn native_target(&self, row: &Value, job: &Value) -> Result<Value> {
        let action = string(&job["action"]);
        let p = &job["payload"];
        if action == "account_template" {
            if !account_templates::eligible(row) || account_templates::passthrough(row) {
                return Err(Error::new(409, "账号继承或透传状态不允许应用模板"));
            }
            let target = self.actions.templates.desired(p)?;
            if account_templates::account_version(row) != job["expected_version"] {
                return Err(Error::new(409, "账号模型配置或凭据已变化"));
            }
            return Ok(json!({"credentials":{"model_mapping":target}}));
        }
        if self.fingerprint(row, job)? != string(&job["expected_version"]) {
            return Err(Error::new(409, "相关字段或账号资格已变化"));
        }
        if action == "proxy" {
            if settings_capabilities(row)["proxy"]["editable"] != true {
                return Err(Error::new(409, "代理继承母账号，不能独立修改"));
            }
            return Ok(
                json!({"proxy_id":if truthy(&p["proxy_id"]) {p["proxy_id"].clone()} else {json!(0)}}),
            );
        }
        if action == "delete" {
            return Ok(json!({}));
        }
        let scope = ids(&p["scope_group_ids"]);
        let selected = ids(&p["group_ids"]);
        if !matches!(string(&row["platform"]), "openai" | "grok")
            || scope.iter().chain(&selected).any(|i| *i < 1)
            || !selected.is_subset(&scope)
        {
            return Err(Error::new(409, "分组范围或平台不匹配"));
        }
        let groups = self
            .actions
            .queries
            .db
            .all(
                "SELECT id,platform FROM groups WHERE deleted_at IS NULL AND id=ANY($1)",
                &[Param::Ints(scope.iter().copied().collect())],
            )
            .await?;
        if groups
            .iter()
            .filter_map(|g| integer(&g["id"]))
            .collect::<BTreeSet<_>>()
            != scope
            || groups.iter().any(|g| g["platform"] != row["platform"])
        {
            return Err(Error::new(409, "分组已变化，请重新预览"));
        }
        Ok(
            json!({"group_ids":ids(&row["group_ids"]).difference(&scope).copied().chain(selected).collect::<BTreeSet<_>>() }),
        )
    }
    pub(crate) fn refresh_chunks(&self, bid: &str) -> Result<()> {
        self.store.transaction(|data| {
            let jobs = &data["jobs"];
            let updates = data["batches"][bid]
                .get("chunks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, chunk)| {
                    if chunk["status"] == "unsupported" {
                        return None;
                    }
                    let states: Vec<_> = chunk["job_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|jid| string(&jobs[string(jid)]["status"]))
                        .collect();
                    if states
                        .iter()
                        .all(|s| !matches!(*s, "running" | "checking" | "queued"))
                    {
                        Some((
                            index,
                            if states.contains(&"needs_confirmation") {
                                "needs_confirmation"
                            } else {
                                "checked"
                            },
                        ))
                    } else if states.contains(&"checking") {
                        Some((index, "checking"))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            for (index, status) in updates {
                data["batches"][bid]["chunks"][index]["status"] = status.into();
            }
            Ok(())
        })
    }
    pub(crate) async fn run_native(&self, bid: &str) -> Result<()> {
        let jobs: Vec<_> = {
            let data = self.store.read_shared()?;
            data["batches"][bid]["job_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|jid| &data["jobs"][string(jid)])
                .filter(|j| native(j))
                .cloned()
                .collect()
        };
        if jobs.is_empty() {
            return Ok(());
        }
        let rows = self.native_rows(&jobs).await?;
        let mut candidates = vec![];
        for job in jobs {
            let row = rows.get(&integer(&job["account_id"]).unwrap());
            let jid = string(&job["id"]);
            if native_reached(row, &job) {
                self.update(jid,json!({"status":"completed","reason":"","result":native_confirmed(row,&job,job["status"]=="queued")}))?;
            } else if job["status"] == "checking" {
                self.update(jid,json!({"status":"needs_confirmation","reason":"已派发请求尚未确认，未重复执行","current":self.current(row.unwrap_or(&json!({})),&job)?}))?;
            } else if row.is_some() {
                candidates.push(job);
            } else {
                self.update(
                    jid,
                    json!({"status":"needs_confirmation","reason":"账号已删除或不存在"}),
                )?;
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }
        let key = self.key(string(&candidates[0]["id"]))?;
        if key.is_empty() {
            for job in candidates {
                self.update(
                    string(&job["id"]),
                    json!({"status":"needs_confirmation","reason":"管理员授权不可用"}),
                )?;
            }
            return Ok(());
        }
        self.actions.upstream.authenticate(&key, true).await?;
        for chunk in candidates.chunks(100) {
            if self.actions.supervisor.is_stopping() {
                return Ok(());
            }
            self.native_chunk(bid, chunk, &key).await?;
        }
        Ok(())
    }
    async fn native_chunk(&self, bid: &str, jobs: &[Value], key: &str) -> Result<()> {
        let rows = self.native_rows(jobs).await?;
        let delete = jobs[0]["action"] == "delete";
        let descendants = if delete {
            self.children(
                &jobs
                    .iter()
                    .filter_map(|j| integer(&j["account_id"]))
                    .collect::<Vec<_>>(),
            )
            .await?
        } else {
            BTreeMap::new()
        };
        let mut needed = BTreeMap::new();
        for job in jobs {
            let aid = integer(&job["account_id"]).unwrap();
            let Some(row) = rows.get(&aid) else {
                continue;
            };
            let parent = integer(&row["parent_account_id"])
                .filter(|id| *id != 0)
                .or_else(|| integer(&row["extra"]["parent_account_id"]))
                .unwrap_or(0);
            let mut ids = BTreeSet::from([aid]);
            if parent != 0 {
                ids.insert(parent);
            }
            ids.extend(descendants.get(&aid).into_iter().flatten());
            needed.insert(string(&job["id"]).to_owned(), ids);
        }
        // Acquire individual members of the union once, so two siblings in a
        // native bulk request share their parent's lease instead of deadlocking.
        let mut held = BTreeMap::new();
        for id in needed.values().flatten().copied().collect::<BTreeSet<_>>() {
            if let Ok(lease) = self.actions.locks.try_ids([id]) {
                held.insert(id, lease);
            }
        }
        let rows = self.native_rows(jobs).await?;
        let descendants = if delete {
            self.children(
                &jobs
                    .iter()
                    .filter_map(|j| integer(&j["account_id"]))
                    .collect::<Vec<_>>(),
            )
            .await?
        } else {
            BTreeMap::new()
        };
        let choices = if jobs[0]["action"] == "proxy" && !jobs[0]["payload"]["proxy_id"].is_null() {
            self.actions.proxies(key).await?
        } else {
            vec![]
        };
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for job in jobs {
            let jid = string(&job["id"]);
            let aid = integer(&job["account_id"]).unwrap();
            if self.store.read_shared()?["jobs"][jid]["status"] != "queued" {
                continue;
            }
            if needed
                .get(jid)
                .is_none_or(|ids| ids.iter().any(|id| !held.contains_key(id)))
            {
                self.update(jid, json!({"reason":"等待账号空闲"}))?;
                continue;
            }
            let Some(row) = rows.get(&aid) else {
                continue;
            };
            let target: Result<Value> = async {
                let parent = integer(&row["parent_account_id"])
                    .filter(|id| *id != 0)
                    .or_else(|| integer(&row["extra"]["parent_account_id"]));
                if parent.is_some_and(|p| p != 0 && !needed[jid].contains(&p)) {
                    return Err(Error::new(409, "账号继承关系已变化"));
                }
                if delete
                    && descendants.get(&aid).cloned().unwrap_or_default()
                        != ids(&job["child_account_ids"])
                            .into_iter()
                            .collect::<Vec<_>>()
                {
                    return Err(Error::new(409, "关联账号已变化，请重新确认删除影响"));
                }
                if job["action"] == "proxy"
                    && !job["payload"]["proxy_id"].is_null()
                    && !choices
                        .iter()
                        .any(|p| p["id"] == job["payload"]["proxy_id"] && p["status"] == "active")
                {
                    return Err(Error::new(409, "所选代理已不可用"));
                }
                self.native_target(row, job).await
            }
            .await;
            match target {
                Err(error) => {
                    self.update(
                        jid,
                        json!({"status":"needs_confirmation","reason":error.detail}),
                    )?;
                }
                Ok(target) => {
                    if native_reached(Some(row), job) {
                        self.update(jid,json!({"status":"completed","reason":"","result":native_confirmed(Some(row),job,true)}))?;
                        if delete {
                            self.notify_deleted_job(job);
                        }
                    } else {
                        let mut job = job.clone();
                        job["native_target"] = target.clone();
                        groups.entry(digest(&target)).or_default().push(job);
                    }
                }
            }
        }
        for jobs in groups.into_values() {
            if delete {
                self.delete_native_chunk(bid, jobs, key).await?;
            } else if self.store.read_shared()?["batches"][bid]["native_unsupported"] == true {
                for job in jobs {
                    self.send_native(bid, vec![job], key, true, false).await?;
                }
            } else {
                self.send_native(bid, jobs, key, false, false).await?;
            }
        }
        drop(held);
        Ok(())
    }
    async fn delete_native_chunk(&self, bid: &str, jobs: Vec<Value>, key: &str) -> Result<()> {
        let rows = self.native_rows(&jobs).await?;
        let children = self
            .children(
                &jobs
                    .iter()
                    .filter_map(|j| integer(&j["account_id"]))
                    .collect::<Vec<_>>(),
            )
            .await?;
        let mut allowed = vec![];
        for job in jobs {
            let aid = integer(&job["account_id"]).unwrap();
            let jid = string(&job["id"]);
            let row = rows.get(&aid);
            if native_reached(row, &job) {
                self.update(jid,json!({"status":"completed","reason":"","result":native_confirmed(row,&job,false)}))?;
                self.notify_deleted_job(&job);
                continue;
            }
            let children = children.get(&aid).cloned().unwrap_or_default();
            if row.is_none()
                || self.fingerprint(row.unwrap(), &job)? != string(&job["expected_version"])
                || children
                    != ids(&job["child_account_ids"])
                        .into_iter()
                        .collect::<Vec<_>>()
            {
                self.update(jid,json!({"status":"needs_confirmation","reason":"账号或关联账号已变化，请重新确认删除"}))?;
            } else {
                allowed.push(job);
            }
        }
        let compatibility = self.store.read_shared()?["batches"][bid]["native_unsupported"] == true;
        let groups = if compatibility {
            allowed.into_iter().map(|j| vec![j]).collect::<Vec<_>>()
        } else {
            vec![allowed]
        };
        for group in groups {
            let selected = self.record_native(bid, &group, compatibility)?;
            if selected.is_empty() {
                continue;
            }
            self.send_native(bid, selected, key, compatibility, true)
                .await?;
        }
        Ok(())
    }
    fn record_native(&self, bid: &str, jobs: &[Value], compatibility: bool) -> Result<Vec<Value>> {
        let cid = id();
        let at = now();
        let mode = if compatibility {
            "compatibility"
        } else {
            "native"
        };
        self.store.transaction(|data| {
            let mut selected = vec![];
            for job in jobs {
                let live = &mut data["jobs"][string(&job["id"])];
                if live["status"] != "queued" {
                    continue;
                }
                live.as_object_mut().ok_or_else(Error::internal)?.extend(
                    json!({"status":"running","started_at":at,"updated_at":at,"reason":"","dispatch_id":cid,"execution_mode":mode,"native_target":job["native_target"]})
                        .as_object().unwrap().clone(),
                );
                selected.push(live.clone());
            }
            if !selected.is_empty() {
                let batch = &mut data["batches"][bid];
                if batch["chunks"].is_null() {
                    batch["chunks"] = json!([]);
                }
                batch["chunks"].as_array_mut().ok_or_else(Error::internal)?.push(
                    json!({"id":cid,"job_ids":selected.iter().map(|j|&j["id"]).collect::<Vec<_>>(),"account_ids":selected.iter().map(|j|&j["account_id"]).collect::<Vec<_>>(),"status":"dispatched","execution_mode":mode,"dispatched_at":at}),
                );
            }
            Ok(selected)
        })
    }
    fn send_native<'a>(
        &'a self,
        bid: &'a str,
        jobs: Vec<Value>,
        key: &'a str,
        compatibility: bool,
        prepared: bool,
    ) -> BoxFuture<'a, Result<()>> {
        async move {
            let selected = if prepared {
                jobs
            } else {
                self.record_native(bid, &jobs, compatibility)?
            };
            if selected.is_empty() {
                return Ok(());
            }
            let action = string(&selected[0]["action"]);
            let expected: BTreeSet<_> = selected
                .iter()
                .filter_map(|j| integer(&j["account_id"]))
                .collect();
            let path = if compatibility {
                format!("accounts/{}", selected[0]["account_id"])
            } else if action == "delete" {
                "accounts/batch-delete".into()
            } else {
                "accounts/bulk-update".into()
            };
            let mut body = selected[0]["native_target"].clone();
            if !compatibility {
                body["account_ids"] = json!(expected);
            }
            let method = if compatibility {
                if action == "delete" {
                    reqwest::Method::DELETE
                } else {
                    reqwest::Method::PUT
                }
            } else {
                reqwest::Method::POST
            };
            let mut parsed = None;
            let mut unavailable = false;
            let mut errors = BTreeMap::new();
            if let Ok(response) = self
                .actions
                .request(
                    method,
                    &path,
                    key,
                    Some(&body),
                    Duration::from_secs(30),
                    false,
                )
                .await
            {
                let status = response.status().as_u16();
                if let Ok(bytes) = read_limited(response, 8_000_000).await {
                    if !compatibility && unsupported(status, &bytes) {
                        unavailable = true;
                    } else if (200..300).contains(&status) {
                        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                            if value["code"].as_i64() == Some(0) {
                                parsed = if compatibility {
                                    Some((expected.clone(), BTreeSet::new()))
                                } else {
                                    result_ids(&value["data"], &expected, action)
                                };
                                if let Some((_, failed)) = &parsed {
                                    for item in value["data"][if action == "delete" {
                                        "errors"
                                    } else {
                                        "results"
                                    }]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    {
                                        if let Some(aid) = integer(&item["account_id"])
                                            .filter(|id| failed.contains(id))
                                        {
                                            errors.insert(aid, safe_error(&item["error"], 200));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if unavailable {
                self.store.transaction(|data| {
                    data["batches"][bid]["native_unsupported"] = true.into();
                    for job in &selected {
                        let value = &mut data["jobs"][string(&job["id"])];
                        value["status"] = "queued".into();
                        value["reason"] = "原生批量接口不支持，兼容执行".into();
                    }
                    for chunk in data["batches"][bid]["chunks"]
                        .as_array_mut()
                        .into_iter()
                        .flatten()
                    {
                        if chunk["id"] == selected[0]["dispatch_id"] {
                            chunk["status"] = "unsupported".into();
                        }
                    }
                    Ok(())
                })?;
                for job in selected {
                    self.send_native(bid, vec![job], key, true, false).await?;
                }
                return Ok(());
            }
            // This durable boundary precedes readback. A DB failure below is an
            // unknown outcome; only a known absent route permits compatibility.
            self.update_many(
                &selected
                    .iter()
                    .map(|job| {
                        (
                            string(&job["id"]),
                            json!({"status":"checking","reason":"正在核对批量结果"}),
                        )
                    })
                    .collect::<Vec<_>>(),
            )?;
            let rows = self.native_rows(&selected).await?;
            let mut updates = Vec::with_capacity(selected.len());
            for job in &selected {
                let aid = integer(&job["account_id"]).unwrap();
                let row = rows.get(&aid);
                let (status, reason, result) = if native_reached(row, job) {
                    (
                        "completed",
                        if compatibility {
                            "兼容执行".to_owned()
                        } else {
                            String::new()
                        },
                        native_confirmed(row, job, false),
                    )
                } else if parsed
                    .as_ref()
                    .is_some_and(|(_, failed)| failed.contains(&aid))
                {
                    (
                        "failed",
                        errors
                            .get(&aid)
                            .cloned()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "Sub2API 未完成此账号操作，请重新预览后重试".into()),
                        Value::Null,
                    )
                } else {
                    (
                        "needs_confirmation",
                        "批量写入结果未确认，未重复执行".into(),
                        Value::Null,
                    )
                };
                updates.push((
                    string(&job["id"]),
                    json!({"status":status,"reason":reason,"result":result}),
                ));
            }
            self.update_many(&updates)?;
            for (job, (_, changes)) in selected.iter().zip(&updates) {
                if action == "delete" && changes["status"] == "completed" {
                    self.notify_deleted_job(job);
                }
                self.audit(job, string(&changes["status"]));
            }
            self.store.transaction(|data| {
                for chunk in data["batches"][bid]["chunks"]
                    .as_array_mut()
                    .into_iter()
                    .flatten()
                {
                    if chunk["id"] == selected[0]["dispatch_id"] {
                        chunk["status"] = "checked".into();
                        chunk["response_valid"] = parsed.is_some().into();
                    }
                }
                Ok(())
            })?;
            Ok(())
        }
        .boxed()
    }
    fn notify_deleted_job(&self, job: &Value) {
        if let Some(aid) = integer(&job["account_id"]) {
            self.backend.account_deleted(aid);
        }
        for child_id in ids(&job["child_account_ids"]) {
            self.backend.account_deleted(child_id);
        }
    }
}
