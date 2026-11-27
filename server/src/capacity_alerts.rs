use crate::{
    compat::{config_revision, datetime, integer, parse_datetime, string, truthy},
    db::Database,
    error::{Error, Result},
    error_evidence,
    quota::number,
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

type SlowPollWatermark = ((i64, i64, i64), i64, i64);
type SlowBackoff = std::sync::Mutex<(u32, Option<(DateTime<Utc>, DateTime<Utc>)>)>;

#[derive(Clone)]
struct SlowPollCache {
    floor_id: i64,
    watermark: SlowPollWatermark,
    store_rev: u64,
    scanned_at: DateTime<Utc>,
    earliest_sample_at: Option<DateTime<Utc>>,
}

struct CursorMemory {
    disk_rev: u64,
    cursor: i64,
    seen: BTreeMap<String, String>,
    last_flush_at: Option<DateTime<Utc>>,
}

fn disk_seen_map(state: &Value) -> BTreeMap<String, String> {
    state["seen"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect()
}

fn sync_cursor_memory<'a>(
    guard: &'a mut Option<CursorMemory>,
    rev: u64,
    state: &Value,
    disk_cursor: i64,
    at: DateTime<Utc>,
) -> &'a mut CursorMemory {
    match guard {
        Some(mem) if mem.disk_rev == rev => {}
        Some(mem) => {
            let mut merged_seen = disk_seen_map(state);
            for (k, v) in std::mem::take(&mut mem.seen) {
                merged_seen
                    .entry(k)
                    .and_modify(|existing| {
                        if v > *existing {
                            *existing = v.clone();
                        }
                    })
                    .or_insert(v);
            }
            mem.cursor = mem.cursor.max(disk_cursor);
            let cur = mem.cursor;
            merged_seen.retain(|k, t| {
                k.parse::<i64>().is_ok_and(|id| id > cur)
                    || parse_datetime(&Value::String(t.clone()))
                        .is_some_and(|ts| ts >= at - Duration::minutes(10))
            });
            mem.seen = merged_seen;
            mem.disk_rev = rev;
        }
        None => {
            *guard = Some(CursorMemory {
                disk_rev: rev,
                cursor: disk_cursor,
                seen: disk_seen_map(state),
                last_flush_at: Some(at),
            });
        }
    }
    guard.as_mut().unwrap()
}

#[derive(Clone)]
pub struct CapacityAlerts {
    pub(crate) clock: crate::compat::WallClock,
    pub store: CapacityStore,
    db: Database,
    audit_path: PathBuf,
    last_seen_cleanup: std::sync::Arc<std::sync::Mutex<Option<DateTime<Utc>>>>,
    cursor_mem: std::sync::Arc<std::sync::Mutex<Option<CursorMemory>>>,
    slow_cache: std::sync::Arc<std::sync::Mutex<Option<SlowPollCache>>>,
    slow_backoff: std::sync::Arc<SlowBackoff>,
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
            cursor_mem: std::sync::Arc::new(std::sync::Mutex::new(None)),
            slow_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            slow_backoff: std::sync::Arc::new(std::sync::Mutex::new((0, None))),
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
    fn effective_cursor(&self, at: DateTime<Utc>) -> Result<i64> {
        let (rev, state) = self.store.shared_snapshot_versioned()?;
        let disk_cursor = integer(&state["cursor"]).ok_or_else(Error::internal)?;
        let mut guard = self.cursor_mem.lock().unwrap_or_else(|e| e.into_inner());
        Ok(sync_cursor_memory(&mut guard, rev, &state, disk_cursor, at).cursor)
    }
    fn effective_cursor_and_seen(&self, at: DateTime<Utc>) -> Result<(i64, Vec<i64>)> {
        let (rev, state) = self.store.shared_snapshot_versioned()?;
        let disk_cursor = integer(&state["cursor"]).ok_or_else(Error::internal)?;
        let mut guard = self.cursor_mem.lock().unwrap_or_else(|e| e.into_inner());
        let mem = sync_cursor_memory(&mut guard, rev, &state, disk_cursor, at);
        let mut seen_ids = mem
            .seen
            .keys()
            .chain(
                state["pending"]
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys()),
            )
            .filter_map(|key| key.parse::<i64>().ok())
            .collect::<Vec<_>>();
        seen_ids.sort_unstable();
        seen_ids.dedup();
        Ok((mem.cursor, seen_ids))
    }
    fn collect_polled(&self, rows: &[Value], advance: bool, at: DateTime<Utc>) -> Result<()> {
        let has_match = {
            let (rev, state) = self.store.shared_snapshot_versioned()?;
            let disk_cursor = integer(&state["cursor"]).unwrap_or(0);
            let mut guard = self.cursor_mem.lock().unwrap_or_else(|e| e.into_inner());
            let mem = sync_cursor_memory(&mut guard, rev, &state, disk_cursor, at);
            let init_at = parse_datetime(&state["initialized_at"]);
            let gateway_since = parse_datetime(&state["gateway_since"]).unwrap_or(at);
            let apikey_since = parse_datetime(&state["apikey_since"]).unwrap_or(at);
            let would_emit = rows.iter().any(|row| {
                let Some(id) = integer(&row["id"]) else {
                    return false;
                };
                let key = id.to_string();
                if mem.seen.contains_key(&key) || state["pending"].get(&key).is_some() {
                    return false;
                }
                if truthy(&row["account_deleted_at"])
                    || error_evidence::match_message(row).is_none()
                {
                    return false;
                }
                let Some(created) = parse_datetime(&row["created_at"]) else {
                    return false;
                };
                if init_at.is_none_or(|t| created < t) {
                    return false;
                }
                if row["error_owner"] == "platform" && created < gateway_since
                    || row["account_type"] == "apikey" && created < apikey_since
                {
                    return false;
                }
                true
            });
            let flush_due = mem
                .last_flush_at
                .is_none_or(|last| at - last >= Duration::seconds(60));
            if !would_emit && !flush_due {
                let stamp = datetime(at);
                let mut max_id = mem.cursor;
                for row in rows {
                    if let Some(id) = integer(&row["id"]) {
                        let key = id.to_string();
                        if !mem.seen.contains_key(&key) && state["pending"].get(&key).is_none() {
                            mem.seen.insert(key, stamp.clone());
                        }
                        if advance && id > max_id {
                            max_id = id;
                        }
                    }
                }
                mem.cursor = max_id;
                let cur = mem.cursor;
                mem.seen.retain(|k, t| {
                    k.parse::<i64>().is_ok_and(|id| id > cur)
                        || parse_datetime(&Value::String(t.clone()))
                            .is_some_and(|ts| ts >= at - Duration::minutes(10))
                });
                return Ok(());
            }
            true
        };
        debug_assert!(has_match);
        let mem_snapshot = self
            .cursor_mem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| (m.cursor, m.seen.clone()));
        self.collect_with_buffered(rows, advance, at, mem_snapshot)?;
        let (new_rev, state) = self.store.shared_snapshot_versioned()?;
        let new_cursor = integer(&state["cursor"]).unwrap_or(0);
        let mut guard = self.cursor_mem.lock().unwrap_or_else(|e| e.into_inner());
        let mem = sync_cursor_memory(&mut guard, new_rev, &state, new_cursor, at);
        mem.last_flush_at = Some(at);
        Ok(())
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
            let (rev, _) = self.store.transaction_versioned(|data| {
                if data["cursor"].is_null() {
                    data["cursor"] = row["id"].clone();
                    data["initialized_at"] = datetime(at).into();
                }
                Ok(())
            })?;
            if let Some(rev) = rev {
                *self.cursor_mem.lock().unwrap_or_else(|e| e.into_inner()) = Some(CursorMemory {
                    disk_rev: rev,
                    cursor: integer(&row["id"]).unwrap_or(0),
                    seen: BTreeMap::new(),
                    last_flush_at: Some(at),
                });
            }
            return Ok(());
        }
        for _ in 0..20 {
            let cursor = self.effective_cursor(at)?;
            let rows = self
                .db
                .all_with_columns(sql::capacity_poll_sql, &[cursor.into()])
                .await?;
            if rows.is_empty() {
                break;
            }
            self.collect_polled(&rows, true, (self.clock)())?;
            if rows.len() < 200 {
                break;
            }
        }
        let since = (at - Duration::minutes(5))
            .max(parse_datetime(&state["initialized_at"]).ok_or_else(Error::internal)?);
        let (upper_cursor, seen_ids) = self.effective_cursor_and_seen(at)?;
        let has_recent = upper_cursor > 0
            && self
                .db
                .one(
                    "SELECT EXISTS (SELECT 1 FROM ops_error_logs WHERE created_at>=$1 AND id<=$2 AND NOT (id = ANY($3))) AS present",
                    &[
                        since.into(),
                        upper_cursor.into(),
                        crate::db::Param::Ints(seen_ids.clone()),
                    ],
                )
                .await?
                .is_some_and(|row| row["present"] == true);
        let mut collected_any = false;
        if has_recent {
            let mut after = 0i64;
            loop {
                let rows = self
                    .db
                    .all_with_columns(
                        sql::capacity_recent_sql,
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
                self.collect_polled(&rows, false, (self.clock)())?;
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
                self.collect_polled(&[], false, at)?;
                *last = Some(at);
            }
        }
        self.poll_slow((self.clock)()).await
    }
    pub fn collect(&self, rows: &[Value], advance: bool, at: DateTime<Utc>) -> Result<()> {
        self.collect_with_buffered(rows, advance, at, None)
    }
    fn collect_with_buffered(
        &self,
        rows: &[Value],
        advance: bool,
        at: DateTime<Utc>,
        buffered: Option<(i64, BTreeMap<String, String>)>,
    ) -> Result<()> {
        let keep_seen = |key: &str, time: &Value, cursor: i64| {
            key.parse::<i64>().is_ok_and(|id| id > cursor)
                || parse_datetime(time).is_some_and(|t| t >= at - Duration::minutes(10))
        };
        {
            let data = self.store.shared_snapshot()?;
            let cursor = integer(&data["cursor"]).unwrap_or(0);
            let buffered_clean = buffered.as_ref().is_none_or(|(buf_cursor, buf_seen)| {
                *buf_cursor <= cursor
                    && buf_seen.iter().all(|(k, v)| {
                        data["seen"].get(k).is_some()
                            || data["pending"].get(k).is_some()
                            || !keep_seen(k, &Value::String(v.clone()), cursor)
                    })
            });
            if buffered_clean
                && data["cursor"].is_i64()
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
            let mut cursor = integer(&data["cursor"]).unwrap_or(0);
            if let Some((buffered_cursor, buffered_seen)) = &buffered {
                if *buffered_cursor > cursor {
                    cursor = *buffered_cursor;
                    data["cursor"] = json!(cursor);
                }
                let map = data["seen"].as_object_mut().unwrap();
                for (k, v) in buffered_seen {
                    let val = Value::String(v.clone());
                    if keep_seen(k, &val, cursor) {
                        map.entry(k.clone()).or_insert(val);
                    }
                }
            }
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
    async fn slow_scan_watermark(&self, at: DateTime<Utc>) -> Option<(i64, SlowPollWatermark)> {
        let row = self
            .db
            .one(
                "SELECT b.floor_id, s.accounts_count, s.accounts_max_id, s.accounts_xor, t.tail_max_id, t.tail_count \
                 FROM ( \
                   SELECT m AS global_max_id, GREATEST(m - 4096, 0)::bigint AS floor_id \
                   FROM (SELECT COALESCE(MAX(id), 0)::bigint AS m FROM usage_logs) raw_max \
                 ) b \
                 CROSS JOIN ( \
                   SELECT COUNT(*)::bigint AS accounts_count, \
                          COALESCE(MAX(id), 0)::bigint AS accounts_max_id, \
                          COALESCE(BIT_XOR(hashint8extended(id, 0)), 0)::bigint AS accounts_xor \
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
        let accounts_sig = (
            integer(&row["accounts_count"]).unwrap_or(0),
            integer(&row["accounts_max_id"]).unwrap_or(0),
            integer(&row["accounts_xor"]).unwrap_or(0),
        );
        let tail_max_id = integer(&row["tail_max_id"]).unwrap_or(0);
        let tail_count = integer(&row["tail_count"]).unwrap_or(0);
        Some((floor_id, (accounts_sig, tail_max_id, tail_count)))
    }
    fn record_slow_failure(&self, at: DateTime<Utc>) {
        let mut backoff = self.slow_backoff.lock().unwrap_or_else(|e| e.into_inner());
        let failures = (backoff.0 + 1).min(5);
        let delay = Duration::seconds((1_i64 << (failures + 1)).min(60));
        *backoff = (failures, Some((at, at + delay)));
        drop(backoff);
        self.audit("slow_ttft_query_error", json!({"error":"DatabaseError"}));
    }
    fn clear_slow_backoff(&self) {
        *self.slow_backoff.lock().unwrap_or_else(|e| e.into_inner()) = (0, None);
    }
    async fn poll_slow(&self, at: DateTime<Utc>) -> Result<()> {
        if let Some((failed_at, retry_at)) = self
            .slow_backoff
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
        {
            if at >= failed_at && at < retry_at {
                return Ok(());
            }
        }
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
                    && at < cached.scanned_at + Duration::seconds(300)
                    && cached
                        .earliest_sample_at
                        .is_none_or(|earliest| earliest >= window_start)
            })
            .cloned();
        let (floor_id, watermark) = if let Some(cached) = valid_cached {
            let check = match self
                .db
                .one(
                    "SELECT b.global_max_id, s.accounts_count, s.accounts_max_id, s.accounts_xor, t.tail_max_id, t.tail_count \
                     FROM (SELECT COALESCE(MAX(id), 0)::bigint AS global_max_id FROM usage_logs) b \
                     CROSS JOIN ( \
                       SELECT COUNT(*)::bigint AS accounts_count, \
                              COALESCE(MAX(id), 0)::bigint AS accounts_max_id, \
                              COALESCE(BIT_XOR(hashint8extended(id, 0)), 0)::bigint AS accounts_xor \
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
                    self.record_slow_failure(at);
                    return Ok(());
                }
            };
            if let (Some(tail_max_id), Some(tail_count)) = (
                integer(&check["tail_max_id"]),
                integer(&check["tail_count"]),
            ) {
                let current_wm = (
                    (
                        integer(&check["accounts_count"]).unwrap_or(0),
                        integer(&check["accounts_max_id"]).unwrap_or(0),
                        integer(&check["accounts_xor"]).unwrap_or(0),
                    ),
                    tail_max_id,
                    tail_count,
                );
                if current_wm == cached.watermark {
                    self.clear_slow_backoff();
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
                            self.record_slow_failure(at);
                            return Ok(());
                        }
                    }
                }
            } else {
                match self.slow_scan_watermark(at).await {
                    Some(v) => v,
                    None => {
                        self.record_slow_failure(at);
                        return Ok(());
                    }
                }
            }
        } else {
            match self.slow_scan_watermark(at).await {
                Some(v) => v,
                None => {
                    self.record_slow_failure(at);
                    return Ok(());
                }
            }
        };
        let rows = match self
            .db
            .all_with_columns(sql::capacity_slow_sql, &[window_start.into(), at.into()])
            .await
        {
            Ok(v) => v,
            Err(_) => {
                self.record_slow_failure(at);
                return Ok(());
            }
        };
        self.clear_slow_backoff();
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

const NON_TEXT: &[&str] = &[
    "/audio", "/images", "/videos", "-image", "-video", "-tts", "-stt", "realtime", "whisper",
    "dall-e",
];

fn text_stream(row: &Value) -> Option<String> {
    if row["stream"] != true {
        return None;
    }
    let model = if truthy(&row["upstream_model"]) {
        string(&row["upstream_model"])
    } else {
        string(&row["model"])
    }
    .trim();
    if model.is_empty()
        || [
            "image_count",
            "image_output_tokens",
            "video_count",
            "video_duration_seconds",
        ]
        .into_iter()
        .any(|k| number(&row[k]).is_some_and(|v| v != 0.0))
    {
        return None;
    }
    let source = format!("{} {}", string(&row["inbound_endpoint"]), model).to_lowercase();
    if NON_TEXT.iter().any(|v| source.contains(v)) {
        return None;
    }
    Some(model.into())
}

#[derive(Clone)]
pub struct SlowSample {
    pub(crate) id: i64,
    pub(crate) at: DateTime<Utc>,
    pub(crate) first: f64,
}

pub fn slow_sample(row: &Value, account: &Value) -> Option<SlowSample> {
    if account["platform"] != "openai" || !matches!(string(&account["type"]), "oauth" | "apikey") {
        return None;
    }
    text_stream(row)?;
    let first = number(&row["first_token_ms"]).filter(|v| *v > 0.0)?;
    number(&row["duration_ms"]).filter(|v| *v > first)?;
    number(&row["output_tokens"]).filter(|v| *v >= 0.0)?;
    Some(SlowSample {
        id: integer(&row["id"]).unwrap_or(0),
        at: parse_datetime(&row["created_at"])?,
        first,
    })
}

pub fn slow_warning(samples: &[SlowSample], at: DateTime<Utc>) -> Option<Value> {
    let mut rows: Vec<_> = samples
        .iter()
        .filter(|v| v.at >= at - Duration::days(1) && v.at <= at)
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse((v.at, v.id)));
    rows.truncate(10);
    if rows.len() < 10 {
        return None;
    }
    let slow = rows.iter().filter(|v| v.first > 10_000.0).count();
    Some(
        json!({"kind":"slow_ttft","sample_count":rows.len(),"slow_count":slow,"threshold_ms":10000,"active":slow >= 8 && rows[0].first > 10_000.0,"latest_sample_id":rows[0].id,"latest_first_token_ms":rows[0].first,"latest_at":datetime(rows[0].at)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn unrelated_store_writes_preserve_unflushed_cursor_seen_and_flush_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapacityStore::new(dir.path().join("capacity-alert-state.json"));
        let t0 = Utc.with_ymd_and_hms(2026, 11, 26, 5, 0, 0).unwrap();
        let (rev1, _) = store
            .transaction_versioned(|data| {
                data["cursor"] = json!(100);
                data["initialized_at"] = datetime(t0).into();
                data["seen"] = json!({"100": datetime(t0)});
                Ok(())
            })
            .unwrap();
        let rev1 = rev1.unwrap();

        let mut guard: Option<CursorMemory> = None;
        let state1 = store.shared_snapshot().unwrap();
        {
            let mem = sync_cursor_memory(&mut guard, rev1, &state1, 100, t0);
            assert_eq!(mem.cursor, 100);
            assert_eq!(mem.last_flush_at, Some(t0));
            // Simulate benign rows advancing in-memory cursor and seen without disk flush
            mem.cursor = 145;
            mem.seen
                .insert("145".into(), datetime(t0 + Duration::seconds(10)));
        }

        // Unrelated update to `marks` / `slow_ttft` bumps disk_rev while disk cursor stays 100
        store
            .transaction(|data| {
                data["marks"]["7"] = json!({
                    "account_id": 7,
                    "marked": true,
                    "version": 1,
                    "changed_at": datetime(t0 + Duration::seconds(20))
                });
                Ok(())
            })
            .unwrap();
        let (rev2, state2) = store.shared_snapshot_versioned().unwrap();
        assert_ne!(rev2, rev1);
        let disk_cursor = integer(&state2["cursor"]).unwrap();
        assert_eq!(disk_cursor, 100);

        let mem = sync_cursor_memory(
            &mut guard,
            rev2,
            &state2,
            disk_cursor,
            t0 + Duration::seconds(25),
        );
        assert_eq!(mem.disk_rev, rev2);
        assert_eq!(mem.cursor, 145, "in-memory cursor must not regress to disk");
        assert!(
            mem.seen.contains_key("145") && mem.seen.contains_key("100"),
            "in-memory seen must merge with disk seen"
        );
        assert_eq!(
            mem.last_flush_at,
            Some(t0),
            "unrelated disk_rev change must not postpone last_flush_at"
        );
    }
}
