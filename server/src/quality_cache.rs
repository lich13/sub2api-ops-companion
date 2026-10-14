use crate::{
    compat::{datetime, digest, integer, pick, string},
    db::{bind, row_value, Database, Param},
    quality::{self, Baselines},
    sql::QUALITY_SUPPORTED,
};
use chrono::{Duration, Utc};
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

struct Cache {
    results: BTreeMap<i64, Value>,
    attempt: Option<Instant>,
    success: Option<Instant>,
    baseline_at: Option<Instant>,
    baselines: Option<Baselines>,
    signature: String,
    failed: bool,
    diagnostic: Value,
    closed: bool,
    task: Option<tokio::task::JoinHandle<()>>,
}
#[derive(Clone)]
pub struct QualityCache {
    db: Database,
    state: Arc<Mutex<Cache>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

#[derive(Debug)]
struct EvidenceError {
    stage: &'static str,
    kind: &'static str,
}
impl EvidenceError {
    fn query(stage: &'static str, error: sqlx::Error) -> Self {
        let timeout = error
            .as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|v| v == "57014");
        Self {
            stage,
            kind: if timeout {
                "query_timeout"
            } else {
                "query_failed"
            },
        }
    }
}

async fn read_evidence(
    db: &Database,
    now: chrono::DateTime<Utc>,
) -> std::result::Result<(Vec<Value>, quality::Evidence, Vec<Value>), EvidenceError> {
    let mut transaction = db
        .pool
        .begin()
        .await
        .map_err(|e| EvidenceError::query("schema", e))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(|e| EvidenceError::query("schema", e))?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *transaction)
        .await
        .map_err(|e| EvidenceError::query("schema", e))?;
    async fn query(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        stage: &'static str,
        sql: &str,
        params: &[Param],
    ) -> std::result::Result<Vec<Value>, EvidenceError> {
        let mut rows = bind(sql, params).fetch(&mut **transaction);
        let mut values = Vec::new();
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|e| EvidenceError::query(stage, e))?
        {
            values.push(row_value(row).map_err(|_| EvidenceError {
                stage,
                kind: "query_failed",
            })?);
        }
        Ok(values)
    }
    let columns = query(&mut transaction,"schema","SELECT attname FROM pg_attribute WHERE attrelid=to_regclass('usage_logs') AND attnum>0 AND NOT attisdropped",&[]).await?;
    let columns: HashSet<&str> = columns.iter().map(|v| string(&v["attname"])).collect();
    let accounts = query(
        &mut transaction,
        "accounts",
        &format!("SELECT id,platform,type FROM accounts WHERE {QUALITY_SUPPORTED}"),
        &[],
    )
    .await?;
    let mut projection: Vec<String> = [
        "id",
        "account_id",
        "created_at",
        "model",
        "input_tokens",
        "output_tokens",
    ]
    .into_iter()
    .map(|v| format!("u.{v}"))
    .collect();
    for field in [
        "upstream_model",
        "cache_read_tokens",
        "cache_creation_tokens",
        "stream",
        "duration_ms",
        "first_token_ms",
        "reasoning_effort",
        "service_tier",
        "openai_ws_mode",
        "inbound_endpoint",
        "image_count",
        "image_output_tokens",
        "video_count",
        "video_duration_seconds",
        "billing_mode",
    ] {
        projection.push(if columns.contains(field) {
            format!("u.{field}")
        } else {
            format!("NULL AS {field}")
        });
    }
    let params = [Param::from(now - Duration::days(7)), Param::from(now)];
    let mut usage = quality::Evidence::new(&accounts, now);
    {
        let sql = format!("SELECT {} FROM usage_logs u JOIN accounts a ON a.id=u.account_id WHERE a.deleted_at IS NULL AND a.platform IN ('openai','grok') AND a.type IN ('oauth','apikey') AND u.created_at >= $1 AND u.created_at <= $2 LIMIT 250001",projection.join(","));
        let mut rows = bind(&sql, &params).fetch(&mut *transaction);
        let mut count = 0;
        while let Some(row) = rows
            .try_next()
            .await
            .map_err(|e| EvidenceError::query("usage", e))?
        {
            count += 1;
            if count > 250_000 {
                return Err(EvidenceError {
                    stage: "usage",
                    kind: "evidence_limit",
                });
            }
            let row = row_value(row).map_err(|_| EvidenceError {
                stage: "usage",
                kind: "query_failed",
            })?;
            usage.push_usage(&row);
            if count % 256 == 0 {
                tokio::task::yield_now().await;
            }
        }
    }
    let errors = query(&mut transaction,"errors",r#"SELECT e.id,e.account_id,e.request_id,e.client_request_id,e.created_at,e.error_owner,e.error_phase,
        e.upstream_status_code,e.status_code,e.provider_error_code,e.provider_error_type,e.error_type,e.network_error_type,
        left(e.error_message,2000) AS error_message,left(e.upstream_error_message,2000) AS upstream_error_message,
        (SELECT jsonb_agg(jsonb_build_object('account_id',x->'account_id','at_unix_ms',x->'at_unix_ms',
         'upstream_status_code',x->'upstream_status_code','kind',x->'kind','message',left(x->>'message',2000)))
         FROM jsonb_array_elements(CASE WHEN jsonb_typeof(e.upstream_errors)='array' THEN e.upstream_errors ELSE '[]'::jsonb END) x) AS upstream_errors
        FROM ops_error_logs e WHERE e.created_at >= $1 AND e.created_at <= $2
        AND (e.account_id IS NOT NULL OR jsonb_array_length(CASE WHEN jsonb_typeof(e.upstream_errors)='array' THEN e.upstream_errors ELSE '[]'::jsonb END)>0) LIMIT 75001"#,&params).await?;
    if errors.len() > 75_000 {
        return Err(EvidenceError {
            stage: "errors",
            kind: "evidence_limit",
        });
    }
    transaction
        .commit()
        .await
        .map_err(|e| EvidenceError::query("errors", e))?;
    Ok((accounts, usage, errors))
}

impl QualityCache {
    pub fn new(db: Database) -> Self {
        Self::new_with_clock(db, Arc::new(Instant::now))
    }

    pub fn new_with_clock(db: Database, clock: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            db,
            clock,
            state: Arc::new(Mutex::new(Cache {
                results: BTreeMap::new(),
                attempt: None,
                success: None,
                baseline_at: None,
                baselines: None,
                signature: String::new(),
                failed: false,
                diagnostic: Value::Null,
                closed: false,
                task: None,
            })),
        }
    }
    async fn refresh(&self) {
        let now = Utc::now();
        match read_evidence(&self.db, now).await {
            Ok((accounts, usage, errors)) => {
                let mut signature: Vec<Value> = accounts
                    .iter()
                    .map(|a| json!([a["id"], a["platform"], a["type"]]))
                    .collect();
                signature.sort_by_key(|v| integer(&v[0]));
                let signature = digest(&json!(signature));
                let (rebuild, baselines) = {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let rebuild = state.baseline_at.is_none_or(|v| {
                        (self.clock)().saturating_duration_since(v) >= StdDuration::from_secs(300)
                    }) || signature != state.signature;
                    (
                        rebuild,
                        if rebuild {
                            None
                        } else {
                            state.baselines.clone()
                        },
                    )
                };
                let computed = tokio::task::spawn_blocking(move || {
                    quality::calculate_evidence(usage, &errors, baselines.as_ref())
                })
                .await;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Ok((results, baselines)) = computed {
                    state.results = results;
                    state.success = Some((self.clock)());
                    state.failed = false;
                    state.diagnostic = Value::Null;
                    if rebuild {
                        state.baselines = Some(baselines);
                        state.baseline_at = Some((self.clock)());
                        state.signature = signature;
                    }
                } else {
                    state.failed = true;
                    state.diagnostic = json!({"stage":"calculation","kind":"calculation_failed","checked_at":datetime(Utc::now())});
                }
            }
            Err(error) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.failed = true;
                state.diagnostic = json!({"stage":error.stage,"kind":error.kind,"checked_at":datetime(Utc::now())});
            }
        }
    }
    pub fn get(&self, ids: &[i64], detail: bool) -> BTreeMap<i64, Value> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let at = (self.clock)();
        if !state.closed
            && state
                .attempt
                .is_none_or(|v| at.saturating_duration_since(v) >= StdDuration::from_secs(30))
            && state.task.as_ref().is_none_or(|t| t.is_finished())
        {
            state.attempt = Some(at);
            let this = self.clone();
            state.task = Some(tokio::spawn(async move {
                this.refresh().await;
            }));
        }
        let status = if state
            .success
            .is_some_and(|v| at.saturating_duration_since(v) > StdDuration::from_secs(300))
        {
            "stale"
        } else if state.failed {
            "delayed"
        } else {
            "fresh"
        };
        ids.iter().map(|id| {
            let mut value = state.results.get(id).cloned().unwrap_or(json!({"account_id":id,"score":null,"grade":"yellow","reasons":["计算中"],"sample_status":"pending","computed_at":null,"warnings":[]}));
            value["data_status"] = status.into(); value["diagnostic"] = state.diagnostic.clone();
            if state.failed { value["reasons"] = json!(["统计更新失败"]); } else if status == "stale" { value["reasons"] = json!(["等待统计更新"]); }
            if status != "fresh" { value["warnings"] = json!([]); }
            if !detail { value = pick(&value,&["score","grade","reasons","sample_status","computed_at","data_status","warnings","diagnostic"]); }
            (*id,value)
        }).collect()
    }
    pub async fn close(&self) {
        let task = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closed = true;
            state.task.take()
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}
