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
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

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
#[derive(Clone)]
pub struct CapacityStore(pub Store);
impl CapacityStore {
    pub fn new(path: PathBuf) -> Self {
        Self(Store::new(
            path,
            json!({"version":1,"cursor":null,"initialized_at":null,"seen":{},"marks":{},"pending":{},"slow_ttft":{},"slow_pending":{},"detection_events":{}}),
            validate,
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
        Ok((*self.shared_snapshot()?).clone())
    }
    pub fn shared_snapshot(&self) -> Result<std::sync::Arc<Value>> {
        Ok(self.shared_snapshot_versioned()?.1)
    }
    pub(crate) fn shared_snapshot_versioned(&self) -> Result<(u64, std::sync::Arc<Value>)> {
        let (rev, mut data) = self.0.read_shared_versioned()?;
        if ["slow_ttft", "slow_pending", "detection_events"]
            .iter()
            .any(|key| data.get(*key).is_none())
        {
            Self::normalize(std::sync::Arc::make_mut(&mut data));
        }
        Ok((rev, data))
    }
    pub fn transaction<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        self.transaction_versioned(action).map(|(_, out)| out)
    }
    pub(crate) fn transaction_versioned<T>(
        &self,
        action: impl FnOnce(&mut Value) -> Result<T>,
    ) -> Result<(Option<u64>, T)> {
        self.0.transaction_versioned(|data| {
            Self::normalize(data);
            let out = action(data)?;
            prune_notifications(data);
            Ok(out)
        })
    }
    pub fn set_mark(
        &self,
        id: i64,
        marked: bool,
        expected: &str,
        at: DateTime<Utc>,
        job: Option<&str>,
    ) -> Result<Value> {
        if id < 1 {
            return Err(Error::invalid());
        }
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
const MAX_TERMINAL_CAPACITY_NOTIFICATIONS: usize = 2_000;

fn prune_notifications(data: &mut Value) {
    let Some(total) = data["notifications"].as_object().map(|m| m.len()) else {
        return;
    };
    if total <= MAX_TERMINAL_CAPACITY_NOTIFICATIONS {
        return;
    }
    let pending = data["pending"].as_object();
    let slow_pending = data["slow_pending"].as_object();
    let detection_events = data["detection_events"].as_object();
    let mut terminals = data["notifications"]
        .as_object()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, (key, entry))| {
            let active_ref = pending.is_some_and(|m| m.contains_key(*key))
                || slow_pending.is_some_and(|m| m.contains_key(*key))
                || detection_events.is_some_and(|m| {
                    m.contains_key(*key) || m.contains_key(&format!("error:{key}"))
                });
            if active_ref {
                return false;
            }
            !matches!(
                entry["status"].as_str(),
                Some("queued" | "retry" | "pending" | "sending")
            )
        })
        .map(|(idx, (key, entry))| {
            let stamp = entry["updated_at"]
                .as_str()
                .or_else(|| entry["at"].as_str())
                .unwrap_or("")
                .to_owned();
            (stamp, idx, key.clone())
        })
        .collect::<Vec<_>>();
    if terminals.len() <= MAX_TERMINAL_CAPACITY_NOTIFICATIONS {
        return;
    }
    terminals.sort();
    let drop_count = terminals.len() - MAX_TERMINAL_CAPACITY_NOTIFICATIONS;
    let map = data["notifications"].as_object_mut().unwrap();
    for (_, _, key) in terminals.into_iter().take(drop_count) {
        map.shift_remove(&key);
    }
}

#[derive(Clone)]
struct SlowPollCache {
    floor_id: i64,
    watermark: (String, i64, i64),
    store_rev: u64,
    scanned_at: DateTime<Utc>,
    earliest_sample_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct CapacityAlerts {
    pub(crate) clock: crate::compat::WallClock,
    pub store: CapacityStore,
    db: Database,
    audit_path: PathBuf,
    last_seen_cleanup: std::sync::Arc<std::sync::Mutex<Option<DateTime<Utc>>>>,
    slow_cache: std::sync::Arc<std::sync::Mutex<Option<SlowPollCache>>>,
}
impl CapacityAlerts {
    pub fn new(db: Database, settings: &crate::settings::Settings) -> Self {
        Self {
            store: CapacityStore::new(
                settings
                    .usage_path
                    .with_file_name("capacity-alert-state.json"),
            ),
            db,
            clock: std::sync::Arc::new(Utc::now),
            audit_path: settings.audit_path.clone(),
            last_seen_cleanup: std::sync::Arc::new(std::sync::Mutex::new(None)),
            slow_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
    fn audit(&self, event: &str, payload: Value) {
        crate::audit::write_at(
            &self.audit_path,
            &format!("capacity_alert_{event}"),
            payload,
            (self.clock)(),
        );
    }
    pub fn initialize(&self, at: DateTime<Utc>) -> Result<()> {
        let state = self.store.shared_snapshot()?;
        if ["gateway_since", "apikey_since", "detection_since"]
            .iter()
            .all(|key| truthy(&state[*key]))
        {
            return Ok(());
        }
        self.store.transaction(|data| {
            for key in ["gateway_since", "apikey_since", "detection_since"] {
                if !truthy(&data[key]) {
                    data[key] = datetime(at).into();
                }
            }
            Ok(())
        })
    }
    pub async fn poll(&self) -> Result<()> {
        let at = (self.clock)();
        let state = self.store.shared_snapshot()?;
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
            let cursor =
                integer(&self.store.shared_snapshot()?["cursor"]).ok_or_else(Error::internal)?;
            let rows = self
                .db
                .all(
                    &format!(
                        "SELECT {} FROM (SELECT * FROM ops_error_logs WHERE id>$1 ORDER BY id LIMIT 200) e CROSS JOIN LATERAL (SELECT to_jsonb(e) AS jb OFFSET 0) ej LEFT JOIN accounts a ON a.id=e.account_id ORDER BY e.id",
                        sql::CAPACITY_FIELDS
                    ),
                    &[cursor.into()],
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            self.collect(&rows, true, (self.clock)())?;
            if rows.len() < 200 {
                break;
            }
        }
        let since = (at - Duration::minutes(5))
            .max(parse_datetime(&state["initialized_at"]).ok_or_else(Error::internal)?);
        let current = self.store.shared_snapshot()?;
        let upper_cursor = integer(&current["cursor"]).unwrap_or(0);
        let has_recent = upper_cursor > 0
            && self
                .db
                .one(
                    "SELECT EXISTS (SELECT 1 FROM ops_error_logs WHERE created_at>=$1 AND id<=$2) AS present",
                    &[since.into(), upper_cursor.into()],
                )
                .await?
                .is_some_and(|row| row["present"] == true);
        let mut collected_any = false;
        if has_recent {
            let mut seen_ids = current["seen"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys())
                .chain(
                    current["pending"]
                        .as_object()
                        .into_iter()
                        .flat_map(|m| m.keys()),
                )
                .filter_map(|key| key.parse::<i64>().ok())
                .collect::<Vec<_>>();
            seen_ids.sort_unstable();
            seen_ids.dedup();
            let mut after = 0i64;
            loop {
                let rows = self
                    .db
                    .all(
                        &format!(
                            "SELECT {} FROM (SELECT * FROM ops_error_logs WHERE created_at>=$1 AND id>$2 AND id<=$4 AND NOT (id = ANY($3)) ORDER BY id LIMIT 200) e CROSS JOIN LATERAL (SELECT to_jsonb(e) AS jb OFFSET 0) ej LEFT JOIN accounts a ON a.id=e.account_id ORDER BY e.id",
                            sql::CAPACITY_FIELDS
                        ),
                        &[
                            since.into(),
                            after.into(),
                            crate::db::Param::Ints(seen_ids.clone()),
                            upper_cursor.into(),
                        ],
                    )
                    .await?;
                if rows.is_empty() {
                    break;
                }
                collected_any = true;
                self.collect(&rows, false, (self.clock)())?;
                after = integer(&rows.last().unwrap()["id"]).ok_or_else(Error::internal)?;
                if rows.len() < 200 {
                    break;
                }
            }
        }
        if !collected_any {
            let mut last = self
                .last_seen_cleanup
                .lock()
                .map_err(|_| Error::internal())?;
            if last.is_none_or(|last| at - last >= Duration::seconds(60)) {
                self.collect(&[], false, at)?;
                *last = Some(at);
            }
        }
        self.poll_slow((self.clock)()).await
    }
    pub fn collect(&self, rows: &[Value], advance: bool, at: DateTime<Utc>) -> Result<()> {
        let keep_seen = |key: &str, time: &Value, cursor: i64| {
            key.parse::<i64>().is_ok_and(|id| id > cursor)
                || parse_datetime(time).is_some_and(|t| t >= at - Duration::minutes(10))
        };
        {
            let data = self.store.shared_snapshot()?;
            let cursor = integer(&data["cursor"]).unwrap_or(0);
            if data["cursor"].is_i64()
                && rows.iter().filter_map(|row| integer(&row["id"])).all(|id| {
                    (!advance || id <= cursor)
                        && (data["seen"].get(id.to_string()).is_some()
                            || data["pending"].get(id.to_string()).is_some())
                })
                && data["seen"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .all(|(key, time)| keep_seen(key, time, cursor))
            {
                return Ok(());
            }
        }
        let emitted = self.store.transaction(|data| {
            let cursor = integer(&data["cursor"]).unwrap_or(0);
            data["seen"]
                .as_object_mut()
                .unwrap()
                .retain(|key, t| keep_seen(key, t, cursor));
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
    async fn slow_scan_watermark(&self, at: DateTime<Utc>) -> Option<(i64, (String, i64, i64))> {
        let row = self
            .db
            .one(
                "SELECT b.floor_id, s.accounts_sig, t.tail_max_id, t.tail_count \
                 FROM ( \
                   SELECT m AS global_max_id, GREATEST(m - 4096, 0)::bigint AS floor_id \
                   FROM (SELECT COALESCE(MAX(id), 0)::bigint AS m FROM usage_logs) raw_max \
                 ) b \
                 CROSS JOIN ( \
                   SELECT md5(COALESCE(jsonb_agg(id ORDER BY id)::text, '[]')) AS accounts_sig \
                   FROM accounts \
                   WHERE deleted_at IS NULL AND platform='openai' AND type IN ('oauth','apikey') \
                 ) s \
                 CROSS JOIN LATERAL ( \
                   SELECT COALESCE(MAX(u.id), 0)::bigint AS tail_max_id, COUNT(*)::bigint AS tail_count \
                   FROM usage_logs u JOIN accounts a ON a.id=u.account_id \
                   WHERE u.id > b.floor_id AND u.created_at <= $1 \
                     AND a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey') \
                 ) t",
                &[at.into()],
            )
            .await
            .ok()??;
        let floor_id = integer(&row["floor_id"]).unwrap_or(0);
        let accounts_sig = row["accounts_sig"].as_str().unwrap_or("").to_owned();
        let tail_max_id = integer(&row["tail_max_id"]).unwrap_or(0);
        let tail_count = integer(&row["tail_count"]).unwrap_or(0);
        Some((floor_id, (accounts_sig, tail_max_id, tail_count)))
    }
    async fn poll_slow(&self, at: DateTime<Utc>) -> Result<()> {
        let (store_rev, previous) = self.store.shared_snapshot_versioned()?;
        let window_start = at - Duration::hours(24);
        let valid_cached = self
            .slow_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|cached| {
                cached.store_rev == store_rev
                    && at >= cached.scanned_at
                    && at < cached.scanned_at + Duration::seconds(30)
                    && cached
                        .earliest_sample_at
                        .is_none_or(|earliest| earliest >= window_start)
            })
            .cloned();
        let (floor_id, watermark) = if let Some(cached) = valid_cached {
            let check = match self
                .db
                .one(
                    "SELECT b.global_max_id, s.accounts_sig, t.tail_max_id, t.tail_count \
                     FROM (SELECT COALESCE(MAX(id), 0)::bigint AS global_max_id FROM usage_logs) b \
                     CROSS JOIN ( \
                       SELECT md5(COALESCE(jsonb_agg(id ORDER BY id)::text, '[]')) AS accounts_sig \
                       FROM accounts \
                       WHERE deleted_at IS NULL AND platform='openai' AND type IN ('oauth','apikey') \
                     ) s \
                     LEFT JOIN LATERAL ( \
                       SELECT COALESCE(MAX(u.id), 0)::bigint AS tail_max_id, COUNT(*)::bigint AS tail_count \
                       FROM usage_logs u JOIN accounts a ON a.id=u.account_id \
                       WHERE u.id > $1 AND u.created_at <= $2 \
                         AND a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey') \
                     ) t ON b.global_max_id >= $1 AND b.global_max_id - $1 <= 20000",
                    &[cached.floor_id.into(), at.into()],
                )
                .await
            {
                Ok(Some(row)) => row,
                _ => {
                    self.audit("slow_ttft_query_error", json!({"error":"DatabaseError"}));
                    return Ok(());
                }
            };
            if let (Some(tail_max_id), Some(tail_count)) = (
                integer(&check["tail_max_id"]),
                integer(&check["tail_count"]),
            ) {
                let current_wm = (
                    check["accounts_sig"].as_str().unwrap_or("").to_owned(),
                    tail_max_id,
                    tail_count,
                );
                if current_wm == cached.watermark {
                    return Ok(());
                }
                let global_max_id = integer(&check["global_max_id"]).unwrap_or(0);
                let next_floor = global_max_id.max(0).saturating_sub(4096);
                if next_floor == cached.floor_id {
                    (next_floor, current_wm)
                } else {
                    match self.slow_scan_watermark(at).await {
                        Some(v) => v,
                        None => {
                            self.audit("slow_ttft_query_error", json!({"error":"DatabaseError"}));
                            return Ok(());
                        }
                    }
                }
            } else {
                match self.slow_scan_watermark(at).await {
                    Some(v) => v,
                    None => {
                        self.audit("slow_ttft_query_error", json!({"error":"DatabaseError"}));
                        return Ok(());
                    }
                }
            }
        } else {
            match self.slow_scan_watermark(at).await {
                Some(v) => v,
                None => {
                    self.audit("slow_ttft_query_error", json!({"error":"DatabaseError"}));
                    return Ok(());
                }
            }
        };
        let rows = match self
            .db
            .all(sql::CAPACITY_SLOW_SQL, &[window_start.into(), at.into()])
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
        let mut earliest_sample_at: Option<DateTime<Utc>> = None;
        for row in rows {
            let Some(id) = integer(&row["account_id"]) else {
                continue;
            };
            if let Some(created) = parse_datetime(&row["created_at"]) {
                earliest_sample_at =
                    Some(earliest_sample_at.map_or(created, |prev| prev.min(created)));
            }
            accounts.insert(id,json!({"id":id,"platform":row["account_platform"],"type":row["account_type"],"name":row.get("account_name").cloned().filter(truthy).unwrap_or(json!(format!("账号 {id}")))}));
            grouped.entry(id).or_default().push(row);
        }
        let projected = slow_projection(&previous, &grouped, &accounts, at);
        if projected["slow_ttft"] == previous["slow_ttft"]
            && projected["alerts"].as_array().unwrap().is_empty()
        {
            *self.slow_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(SlowPollCache {
                floor_id,
                watermark,
                store_rev,
                scanned_at: at,
                earliest_sample_at,
            });
            return Ok(());
        }
        let (updated_rev, matched) = self.store.transaction_versioned(|data| {
            // Recalculate under the file lock: a manual mark or another scan may
            // have changed eligibility since the shared snapshot was read.
            let projected = slow_projection(data, &grouped, &accounts, at);
            data["slow_ttft"] = projected["slow_ttft"].clone();
            for alert in projected["alerts"].as_array().unwrap() {
                let key = alert["key"].as_str().unwrap();
                if !alert["event"].is_null() {
                    data["detection_events"][key] = alert["event"].clone();
                }
                if !data["notifications"].is_object() {
                    data["notifications"] = json!({});
                }
                data["notifications"][key] = alert["notification"].clone();
            }
            Ok(projected["alerts"].clone())
        })?;
        if let Some(updated_rev) = updated_rev {
            *self.slow_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(SlowPollCache {
                floor_id,
                watermark,
                store_rev: updated_rev,
                scanned_at: at,
                earliest_sample_at,
            });
        }
        for alert in matched.as_array().unwrap() {
            self.audit("slow_ttft_matched", alert["audit"].clone());
        }
        Ok(())
    }
}

fn slow_projection(
    data: &Value,
    grouped: &BTreeMap<i64, Vec<Value>>,
    accounts: &BTreeMap<i64, Value>,
    at: DateTime<Utc>,
) -> Value {
    let mut states = data["slow_ttft"].clone();
    if !states.is_object() {
        states = json!({});
    }
    states.as_object_mut().unwrap().retain(|key, _| {
        key.parse::<i64>()
            .is_ok_and(|id| accounts.contains_key(&id))
    });
    let mut alerts = Vec::new();
    for (id, rows) in grouped {
        let samples = rows
            .iter()
            .filter_map(|row| slow_sample(row, &accounts[id]))
            .collect::<Vec<_>>();
        let warning = slow_warning(&samples, at);
        let key = id.to_string();
        let mut state = states
            .get(&key)
            .cloned()
            .unwrap_or(json!({"active":false,"alerted":false}));
        let prior = parse_datetime(if truthy(&state["last_sample_at"]) {
            &state["last_sample_at"]
        } else {
            &state["last_at"]
        });
        let prior_id = integer(&state["last_sample_id"]).unwrap_or(0);
        let fast = samples
            .iter()
            .filter(|s| s.first <= 10000.0 && prior.is_none_or(|p| (s.at, s.id) > (p, prior_id)))
            .max_by_key(|s| (s.at, s.id));
        state["active"] = warning.as_ref().is_some_and(|v| v["active"] == true).into();
        if let Some(newest) = samples.iter().max_by_key(|s| (s.at, s.id)) {
            state["last_sample_id"] = newest.id.into();
            state["last_sample_at"] = datetime(newest.at).into();
        }
        if let Some(fast) = fast {
            state["alerted"] = false.into();
            state["reset_sample_id"] = fast.id.into();
        }
        if state["active"] == true && state["alerted"] != true {
            let warning = warning.unwrap();
            let event_key = format!("slow:{id}:{}", warning["latest_sample_id"]);
            let mark = &data["marks"][&key];
            let created = parse_datetime(&warning["latest_at"]);
            let changed = parse_datetime(&mark["changed_at"]);
            let allowed = parse_datetime(&data["detection_since"])
                .zip(created)
                .is_some_and(|(since, t)| {
                    t > since && !truthy(&mark["marked"]) && changed.is_none_or(|c| t > c)
                });
            let reason = if allowed {
                Value::Null
            } else if truthy(&mark["marked"]) {
                json!("degradation_mark")
            } else {
                json!("historical")
            };
            let event = if allowed && data["detection_events"].get(&event_key).is_none() {
                json!({"account_id":id,"created_at":datetime(created.unwrap())})
            } else {
                Value::Null
            };
            state["alerted"] = true.into();
            state["last_at"] = warning["latest_at"].clone();
            alerts.push(json!({"key":event_key,"event":event,"notification":{"status":if allowed {"clue"} else {"suppressed"},"reason":reason,"at":datetime(at)},"audit":{"account_id":id,"sample_id":warning["latest_sample_id"]}}));
        }
        states[key] = state;
    }
    json!({"slow_ttft":states,"alerts":alerts})
}
