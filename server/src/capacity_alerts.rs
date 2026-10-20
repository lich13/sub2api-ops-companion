use crate::{
    compat::{config_revision, datetime, integer, parse_datetime, truthy},
    db::Database,
    error::{Error, Result},
    error_evidence,
    slow_ttft::{slow_sample, slow_warning},
    sql,
    store::Store,
};
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::PathBuf,
};

pub fn mark_view(id: i64, mark: &Value) -> Value {
    let mark = if truthy(mark) {
        mark.clone()
    } else {
        json!({})
    };
    json!({"marked":mark.get("marked").cloned().unwrap_or(json!(false)),"marked_at":mark["marked_at"],"version":config_revision(&json!([id,mark]))})
}
pub fn validate(data: &Value) -> Result<()> {
    let fail = || Error::new(503, "告警状态无效");
    if !data.is_object() || data["version"] != 1 {
        return Err(fail());
    }
    for key in ["seen", "marks", "pending"] {
        if !data[key].is_object() {
            return Err(fail());
        }
    }
    for key in [
        "slow_ttft",
        "slow_pending",
        "detection_events",
        "notifications",
        "profile_intents",
    ] {
        if data.get(key).is_some_and(|v| !v.is_object()) {
            return Err(fail());
        }
    }
    if truthy(&data["gateway_since"]) && parse_datetime(&data["gateway_since"]).is_none() {
        return Err(fail());
    }
    if !data["cursor"].is_null()
        && (data["cursor"].as_i64().is_none_or(|v| v < 0)
            || parse_datetime(&data["initialized_at"]).is_none())
    {
        return Err(fail());
    }
    for mark in data["marks"].as_object().unwrap().values() {
        if !mark.is_object()
            || !mark["marked"].is_boolean()
            || parse_datetime(&mark["changed_at"]).is_none()
        {
            return Err(fail());
        }
    }
    if data["seen"]
        .as_object()
        .unwrap()
        .values()
        .any(|v| parse_datetime(v).is_none())
    {
        return Err(fail());
    }
    for event in data["pending"].as_object().unwrap().values().chain(
        data["slow_pending"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values()),
    ) {
        if !event.is_object()
            || event["account_id"].as_i64().is_none_or(|v| v < 1)
            || !event["id"].is_i64()
            || !event["attempts"].is_i64()
            || parse_datetime(&event["next_at"]).is_none()
            || parse_datetime(&event["created_at"]).is_none()
            || !event["account_name"].is_string()
        {
            return Err(fail());
        }
    }
    for event in data["pending"].as_object().unwrap().values() {
        if ["requested_model", "upstream_model", "message"]
            .iter()
            .any(|k| !event[*k].is_string())
        {
            return Err(fail());
        }
    }
    for state in data["slow_ttft"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        if !state.is_object()
            || !state["active"].is_boolean()
            || state.get("alerted").is_some_and(|v| !v.is_boolean())
            || ["last_sample_id", "reset_sample_id"]
                .iter()
                .any(|k| !state[*k].is_null() && !state[*k].is_i64())
        {
            return Err(fail());
        }
    }
    for event in data["slow_pending"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        if event["kind"] != "slow_ttft"
            || !event["sample_count"].is_i64()
            || !event["slow_count"].is_i64()
            || !event["account_type"].is_string()
        {
            return Err(fail());
        }
    }
    Ok(())
}
pub struct MarkGuard {
    _file: File,
}
#[derive(Clone)]
pub struct CapacityStore(pub Store);
impl CapacityStore {
    pub fn new(path: PathBuf, read_only: bool) -> Self {
        Self(Store::new(
            path,
            json!({"version":1,"cursor":null,"initialized_at":null,"seen":{},"marks":{},"pending":{},"slow_ttft":{},"slow_pending":{},"detection_events":{}}),
            validate,
            read_only,
        ))
    }
    fn normalize(data: &mut Value) {
        for key in ["slow_ttft", "slow_pending", "detection_events"] {
            if data.get(key).is_none() {
                data[key] = json!({});
            }
        }
    }
    pub fn snapshot(&self) -> Result<Value> {
        let mut data = self.0.read()?;
        Self::normalize(&mut data);
        Ok(data)
    }
    pub fn transaction<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        self.0.transaction(|data| {
            Self::normalize(data);
            action(data)
        })
    }
    pub fn account_guard(&self, id: i64) -> Result<MarkGuard> {
        if self.0.is_read_only() {
            return Err(Error::read_only());
        }
        if id < 1 {
            return Err(Error::invalid());
        }
        let parent = self
            .0
            .path()
            .parent()
            .ok_or_else(Error::internal)?
            .join("capacity-alert-locks");
        std::fs::create_dir_all(&parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(parent.join(format!("{id}.lock")))?;
        file.lock_exclusive()?;
        crate::store::private_permissions(&file)?;
        Ok(MarkGuard { _file: file })
    }
    pub fn set_mark(
        &self,
        id: i64,
        marked: bool,
        expected: &str,
        at: DateTime<Utc>,
        job: Option<&str>,
    ) -> Result<Value> {
        let _guard = self.account_guard(id)?;
        self.set_mark_guarded(id, marked, expected, at, job)
    }
    pub fn set_mark_guarded(
        &self,
        id: i64,
        marked: bool,
        expected: &str,
        at: DateTime<Utc>,
        job: Option<&str>,
    ) -> Result<Value> {
        self.transaction(|data| {
            let key=id.to_string();if mark_view(id,&data["marks"][&key])["version"]!=expected {return Err(Error::new(409,"降智标记已变化，请刷新后重试"));}
            let stamp=datetime(at);let mut mark=json!({"marked":marked,"marked_at":if marked {json!(stamp)} else {Value::Null},"changed_at":stamp});if let Some(job)=job {mark["detection_job_id"]=job.into();}data["marks"][&key]=mark.clone();
            for field in ["pending","slow_pending"] {
                let keys=data[field].as_object().unwrap().iter().filter(|(_,e)|integer(&e["account_id"])==Some(id)).map(|(key,_)|key.clone()).collect::<Vec<_>>();
                if data.get("notifications").is_none() {data["notifications"]=json!({});}
                for key in keys {data["notifications"][&key]=json!({"status":"suppressed","reason":if marked {"degradation_mark"} else {"mark_changed"},"at":stamp});data[field].as_object_mut().unwrap().shift_remove(&key);}
            }
            if marked && truthy(&data["slow_ttft"][&key]) {data["slow_ttft"][&key]["alerted"]=true.into();}Ok(mark_view(id,&mark))
        })
    }
}
#[derive(Clone)]
pub struct CapacityAlerts {
    pub store: CapacityStore,
    db: Database,
    audit_path: PathBuf,
    read_only: bool,
}
impl CapacityAlerts {
    pub fn new(db: Database, settings: &crate::settings::Settings) -> Self {
        Self {
            store: CapacityStore::new(
                settings
                    .usage_path
                    .with_file_name("capacity-alert-state.json"),
                settings.read_only,
            ),
            db,
            audit_path: settings.audit_path.clone(),
            read_only: settings.read_only,
        }
    }
    fn audit(&self, event: &str, payload: Value) {
        crate::audit::write(
            &self.audit_path,
            &format!("capacity_alert_{event}"),
            payload,
            self.read_only,
        );
    }
    pub fn retire_legacy(&self, at: DateTime<Utc>) -> Result<()> {
        if truthy(&self.store.snapshot()?["clue_mode_since"]) {
            return Ok(());
        }
        self.store.transaction(|data| {
            if truthy(&data["clue_mode_since"]) {
                return Ok(());
            }
            let stamp = datetime(at);
            let keys = data["pending"]
                .as_object()
                .unwrap()
                .keys()
                .chain(data["slow_pending"].as_object().unwrap().keys())
                .cloned()
                .collect::<Vec<_>>();
            if data.get("notifications").is_none() {
                data["notifications"] = json!({});
            }
            for key in keys {
                data["notifications"][key] =
                    json!({"status":"retired","reason":"clue_only","at":stamp});
            }
            for key in ["pending", "slow_pending", "detection_events"] {
                data[key] = json!({});
            }
            data["clue_mode_since"] = stamp.clone().into();
            data["detection_since"] = stamp.into();
            Ok(())
        })
    }
    pub async fn poll(&self) -> Result<()> {
        if self.read_only {
            return Err(Error::read_only());
        }
        let at = Utc::now();
        self.retire_legacy(at)?;
        let state = self.store.snapshot()?;
        for key in ["gateway_since", "apikey_since"] {
            if !truthy(&state[key]) {
                self.store.transaction(|data| {
                    if data.get(key).is_none() {
                        data[key] = datetime(at).into();
                    }
                    Ok(())
                })?;
            }
        }
        if state["cursor"].is_null() {
            let row = self
                .db
                .one("SELECT coalesce(max(id),0) AS id FROM ops_error_logs", &[])
                .await?
                .ok_or_else(Error::internal)?;
            self.store.transaction(|data| {
                if data["cursor"].is_null() {
                    data["cursor"] = row["id"].clone();
                    data["initialized_at"] = datetime(at).into();
                }
                Ok(())
            })?;
            return Ok(());
        }
        for _ in 0..20 {
            let cursor = integer(&self.store.snapshot()?["cursor"]).ok_or_else(Error::internal)?;
            let rows=self.db.all(&format!("SELECT {} FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id WHERE e.id>$1 ORDER BY e.id LIMIT 200",sql::CAPACITY_FIELDS),&[cursor.into()]).await?;
            if rows.is_empty() {
                break;
            }
            self.collect(&rows, true, Utc::now())?;
            if rows.len() < 200 {
                break;
            }
        }
        let since = (at - Duration::minutes(5))
            .max(parse_datetime(&state["initialized_at"]).ok_or_else(Error::internal)?);
        let mut after = 0i64;
        loop {
            let rows=self.db.all(&format!("SELECT {} FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id WHERE e.created_at>=$1 AND e.id>$2 ORDER BY e.id LIMIT 200",sql::CAPACITY_FIELDS),&[since.into(),after.into()]).await?;
            if rows.is_empty() {
                break;
            }
            self.collect(&rows, false, Utc::now())?;
            after = integer(&rows.last().unwrap()["id"]).ok_or_else(Error::internal)?;
            if rows.len() < 200 {
                break;
            }
        }
        self.poll_slow(Utc::now()).await
    }
    pub fn collect(&self, rows: &[Value], advance: bool, at: DateTime<Utc>) -> Result<()> {
        let emitted = self.store.transaction(|data| {
            let cursor = integer(&data["cursor"]).unwrap_or(0);
            data["seen"].as_object_mut().unwrap().retain(|key, t| {
                key.parse::<i64>().is_ok_and(|id| id > cursor)
                    || parse_datetime(t).is_some_and(|t| t >= at - Duration::minutes(10))
            });
            let mut emitted = vec![];
            for row in rows {
                let Some(id) = integer(&row["id"]) else {
                    continue;
                };
                let key = id.to_string();
                if data["seen"].get(&key).is_some() || data["pending"].get(&key).is_some() {
                    continue;
                }
                data["seen"][&key] = datetime(at).into();
                if truthy(&row["account_deleted_at"])
                    || error_evidence::match_message(row).is_none()
                {
                    continue;
                }
                let Some(created) = parse_datetime(&row["created_at"]) else {
                    continue;
                };
                if parse_datetime(&data["initialized_at"]).is_none_or(|t| created < t) {
                    continue;
                }
                if row["error_owner"] == "platform"
                    && created < parse_datetime(&data["gateway_since"]).unwrap_or(at)
                    || row["account_type"] == "apikey"
                        && created < parse_datetime(&data["apikey_since"]).unwrap_or(at)
                {
                    continue;
                }
                let aid = integer(&row["account_id"]).ok_or_else(Error::internal)?;
                let mark = &data["marks"][aid.to_string()];
                let marked = truthy(&mark["marked"]);
                let changed = parse_datetime(&mark["changed_at"]);
                let suppressed = marked || changed.is_some_and(|t| created <= t);
                let reason = if marked {
                    json!("degradation_mark")
                } else if changed.is_some_and(|t| created <= t) {
                    json!("mark_changed")
                } else {
                    Value::Null
                };
                if parse_datetime(&data["detection_since"]).is_some_and(|t| created > t)
                    && !suppressed
                {
                    let event_key = format!("error:{key}");
                    if data["detection_events"].get(&event_key).is_none() {
                        data["detection_events"][event_key] =
                            json!({"account_id":aid,"created_at":datetime(created)});
                    }
                }
                if data.get("notifications").is_none() {
                    data["notifications"] = json!({});
                }
                let status = if suppressed { "suppressed" } else { "clue" };
                data["notifications"][key] =
                    json!({"status":status,"reason":reason,"at":datetime(at)});
                emitted.push((status, id, aid, reason));
            }
            if advance {
                data["cursor"] = json!(cursor.max(
                    rows.iter()
                        .filter_map(|r| integer(&r["id"]))
                        .max()
                        .unwrap_or(cursor)
                ));
            }
            Ok(emitted)
        })?;
        for (status, error_id, account_id, reason) in emitted {
            self.audit(
                "matched",
                json!({"error_id":error_id,"account_id":account_id}),
            );
            self.audit(
                status,
                json!({"error_id":error_id,"account_id":account_id,"reason":reason}),
            );
        }
        Ok(())
    }
    async fn poll_slow(&self, at: DateTime<Utc>) -> Result<()> {
        let rows = match self
            .db
            .all(
                sql::CAPACITY_SLOW_SQL,
                &[(at - Duration::hours(24)).into(), at.into()],
            )
            .await
        {
            Ok(v) => v,
            Err(_) => {
                self.audit("slow_ttft_query_error", json!({"error":"DatabaseError"}));
                return Ok(());
            }
        };
        let mut grouped: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
        let mut accounts = BTreeMap::new();
        for row in rows {
            let Some(id) = integer(&row["account_id"]) else {
                continue;
            };
            accounts.insert(id,json!({"id":id,"platform":row["account_platform"],"type":row["account_type"],"name":row.get("account_name").cloned().filter(truthy).unwrap_or(json!(format!("账号 {id}")))}));
            grouped.entry(id).or_default().push(row);
        }
        self.store.transaction(|data| {
            data["slow_ttft"].as_object_mut().unwrap().retain(|key,_|key.parse::<i64>().is_ok_and(|id|accounts.contains_key(&id)));
            for (id,rows) in grouped {
                let samples=rows.iter().filter_map(|row|slow_sample(row,&accounts[&id])).collect::<Vec<_>>();let warning=slow_warning(&samples,at);let key=id.to_string();let mut state=data["slow_ttft"].get(&key).cloned().unwrap_or(json!({"active":false,"alerted":false}));
                let prior=parse_datetime(if truthy(&state["last_sample_at"]) {&state["last_sample_at"]} else {&state["last_at"]});let prior_id=integer(&state["last_sample_id"]).unwrap_or(0);
                let fast=samples.iter().filter(|s|s.first<=10000.0 && prior.is_none_or(|p|(s.at,s.id)>(p,prior_id))).max_by_key(|s|(s.at,s.id));state["active"]=warning.as_ref().is_some_and(|v|v["active"]==true).into();
                if let Some(newest) = samples.iter().max_by_key(|s| (s.at,s.id)) {
                    state["last_sample_id"] = newest.id.into();
                    state["last_sample_at"] = datetime(newest.at).into();
                }
                if let Some(fast) = fast {
                    state["alerted"] = false.into();
                    state["reset_sample_id"] = fast.id.into();
                }
                if state["active"]==true && state["alerted"]!=true {
                    let warning=warning.unwrap();let event_key=format!("slow:{id}:{}",warning["latest_sample_id"]);let mark=&data["marks"][&key];let created=parse_datetime(&warning["latest_at"]);let changed=parse_datetime(&mark["changed_at"]);let allowed=parse_datetime(&data["detection_since"]).zip(created).is_some_and(|(since,t)|t>since && !truthy(&mark["marked"]) && changed.is_none_or(|c|t>c));
                    let reason=if allowed {Value::Null} else if truthy(&mark["marked"]) {json!("degradation_mark")} else {json!("historical")};
                    if allowed && data["detection_events"].get(&event_key).is_none() {data["detection_events"][&event_key]=json!({"account_id":id,"created_at":datetime(created.unwrap())});}
                    state["alerted"]=true.into();state["last_at"]=warning["latest_at"].clone();if data.get("notifications").is_none() {data["notifications"]=json!({});}data["notifications"][event_key]=json!({"status":if allowed {"clue"} else {"suppressed"},"reason":reason,"at":datetime(at)});self.audit("slow_ttft_matched",json!({"account_id":id,"sample_id":warning["latest_sample_id"]}));
                }
                data["slow_ttft"][key]=state;
            }Ok(())
        })
    }
}
