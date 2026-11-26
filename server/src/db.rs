use crate::{
    compat,
    error::{Error, Result},
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde_json::{json, Map, Value};
use sqlx::{
    postgres::{PgArguments, PgPoolOptions, PgRow, PgValueFormat},
    query::Query,
    Column, PgPool, Postgres, Row, TypeInfo, ValueRef,
};
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionalColumns {
    pub account_parent_id: bool,
    pub account_session_window_end: bool,
    pub account_session_window_status: bool,
    pub error_upstream_errors: bool,
    pub error_body: bool,
    pub error_upstream_detail: bool,
    pub usage_video_count: bool,
    pub usage_video_duration: bool,
}

impl OptionalColumns {
    pub(crate) fn to_bits(self) -> u8 {
        (u8::from(self.account_parent_id))
            | (u8::from(self.account_session_window_end) << 1)
            | (u8::from(self.account_session_window_status) << 2)
            | (u8::from(self.error_upstream_errors) << 3)
            | (u8::from(self.error_body) << 4)
            | (u8::from(self.error_upstream_detail) << 5)
            | (u8::from(self.usage_video_count) << 6)
            | (u8::from(self.usage_video_duration) << 7)
    }

    pub(crate) fn from_bits(bits: u8) -> Self {
        Self {
            account_parent_id: (bits & 1) != 0,
            account_session_window_end: (bits & (1 << 1)) != 0,
            account_session_window_status: (bits & (1 << 2)) != 0,
            error_upstream_errors: (bits & (1 << 3)) != 0,
            error_body: (bits & (1 << 4)) != 0,
            error_upstream_detail: (bits & (1 << 5)) != 0,
            usage_video_count: (bits & (1 << 6)) != 0,
            usage_video_duration: (bits & (1 << 7)) != 0,
        }
    }

    fn all_present(self) -> bool {
        self.to_bits() == u8::MAX
    }
}

const OPTIONAL_COLUMNS_SQL: &str = "\
SELECT \
  pg_catalog.to_regclass('accounts') IS NOT NULL AS accounts_present, \
  pg_catalog.to_regclass('ops_error_logs') IS NOT NULL AS errors_present, \
  pg_catalog.to_regclass('usage_logs') IS NOT NULL AS usage_present, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('accounts') AND attname = 'parent_account_id' AND attnum > 0 AND NOT attisdropped) AS account_parent_id, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('accounts') AND attname = 'session_window_end' AND attnum > 0 AND NOT attisdropped) AS account_session_window_end, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('accounts') AND attname = 'session_window_status' AND attnum > 0 AND NOT attisdropped) AS account_session_window_status, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('ops_error_logs') AND attname = 'upstream_errors' AND attnum > 0 AND NOT attisdropped) AS error_upstream_errors, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('ops_error_logs') AND attname = 'error_body' AND attnum > 0 AND NOT attisdropped) AS error_body, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('ops_error_logs') AND attname = 'upstream_error_detail' AND attnum > 0 AND NOT attisdropped) AS error_upstream_detail, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('usage_logs') AND attname = 'video_count' AND attnum > 0 AND NOT attisdropped) AS usage_video_count, \
  EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('usage_logs') AND attname = 'video_duration_seconds' AND attnum > 0 AND NOT attisdropped) AS usage_video_duration";

fn is_undefined_column(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("42703"))
}

#[derive(Clone)]
pub struct Database {
    pub pool: PgPool,
    columns_epoch: Instant,
    columns_fast: Arc<AtomicU64>,
    columns_cache: Arc<tokio::sync::Mutex<Option<(Instant, OptionalColumns)>>>,
}

#[derive(Clone, Debug)]
pub enum Param {
    Text(Option<String>),
    Int(Option<i64>),
    Bool(Option<bool>),
    Float(Option<f64>),
    Json(Value),
    Ints(Vec<i64>),
    Texts(Vec<String>),
    Time(Option<DateTime<Utc>>),
}
impl From<&str> for Param {
    fn from(v: &str) -> Self {
        Self::Text(Some(v.to_owned()))
    }
}
impl From<String> for Param {
    fn from(v: String) -> Self {
        Self::Text(Some(v))
    }
}
impl From<i64> for Param {
    fn from(v: i64) -> Self {
        Self::Int(Some(v))
    }
}
impl From<bool> for Param {
    fn from(v: bool) -> Self {
        Self::Bool(Some(v))
    }
}
impl From<DateTime<Utc>> for Param {
    fn from(v: DateTime<Utc>) -> Self {
        Self::Time(Some(v))
    }
}

pub fn bind<'q>(sql: &'q str, params: &[Param]) -> Query<'q, Postgres, PgArguments> {
    let mut query = sqlx::query(sql);
    for param in params {
        query = match param {
            Param::Text(v) => query.bind(v.clone()),
            Param::Int(v) => query.bind(*v),
            Param::Bool(v) => query.bind(*v),
            Param::Float(v) => query.bind(*v),
            Param::Json(v) => query.bind(sqlx::types::Json(v.clone())),
            Param::Ints(v) => query.bind(v.clone()),
            Param::Texts(v) => query.bind(v.clone()),
            Param::Time(v) => query.bind(*v),
        };
    }
    query
}

pub fn row_value(row: PgRow) -> Result<Value> {
    let mut value = Map::new();
    for (i, column) in row.columns().iter().enumerate() {
        let field = if row.try_get_raw(i)?.is_null() {
            Value::Null
        } else {
            match column.type_info().name() {
                "BOOL" => json!(row.try_get::<bool, _>(i)?),
                "INT2" => json!(row.try_get::<i16, _>(i)?),
                "INT4" => json!(row.try_get::<i32, _>(i)?),
                "INT8" => json!(row.try_get::<i64, _>(i)?),
                "FLOAT4" => json!(row.try_get::<f32, _>(i)?),
                "FLOAT8" => json!(row.try_get::<f64, _>(i)?),
                // Decimal text, including its scale, is part of persisted version hashes.
                "NUMERIC" => {
                    let raw = row.try_get_raw(i)?;
                    let is_binary = raw.format() == PgValueFormat::Binary;
                    let dscale = if is_binary {
                        let bytes = raw.as_bytes().map_err(|_| Error::internal())?;
                        let sign_bytes = bytes.get(4..6).ok_or_else(Error::internal)?;
                        let sign = u16::from_be_bytes([sign_bytes[0], sign_bytes[1]]);
                        if !matches!(sign, 0x0000 | 0x4000) {
                            return Err(Error::internal());
                        }
                        let scale = bytes.get(6..8).ok_or_else(Error::internal)?;
                        Some(u16::from_be_bytes([scale[0], scale[1]]) & 0x3FFF)
                    } else {
                        let s = raw.as_str().map_err(|_| Error::internal())?.trim();
                        if s.eq_ignore_ascii_case("nan")
                            || s.eq_ignore_ascii_case("infinity")
                            || s.eq_ignore_ascii_case("+infinity")
                            || s.eq_ignore_ascii_case("-infinity")
                        {
                            return Err(Error::internal());
                        }
                        None
                    };
                    let mut amount = row.try_get::<sqlx::types::BigDecimal, _>(i)?;
                    if let Some(dscale) = dscale {
                        // SQLx reconstructs a base-10000 value but discards the
                        // display scale. Preserve PostgreSQL's decimal places,
                        // including zero, for stable values and hashes.
                        amount = amount.with_scale(i64::from(dscale));
                    }
                    json!(crate::usage_records::decimal_text(&amount, false))
                }
                "JSON" | "JSONB" => row.try_get::<Value, _>(i)?,
                "TIMESTAMPTZ" => json!(
                    compat::datetime(row.try_get::<DateTime<Utc>, _>(i)?).replacen('T', " ", 1)
                ),
                "TIMESTAMP" => json!(row.try_get::<NaiveDateTime, _>(i)?.to_string()),
                "DATE" => json!(row.try_get::<NaiveDate, _>(i)?.to_string()),
                "INT8[]" => json!(row.try_get::<Vec<i64>, _>(i)?),
                "INT4[]" => json!(row.try_get::<Vec<i32>, _>(i)?),
                "TEXT[]" | "VARCHAR[]" => json!(row.try_get::<Vec<String>, _>(i)?),
                "UUID" => json!(row.try_get::<uuid::Uuid, _>(i)?.to_string()),
                "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "UNKNOWN" => {
                    json!(row.try_get::<String, _>(i)?)
                }
                _ => return Err(Error::new(500, "数据库字段类型不兼容")),
            }
        };
        value.insert(column.name().to_owned(), field);
    }
    Ok(Value::Object(value))
}

impl Database {
    pub async fn open(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(16)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(120))
            .max_lifetime(Duration::from_secs(1800))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET TIME ZONE 'UTC'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        let db = Self {
            pool,
            columns_epoch: Instant::now(),
            columns_fast: Arc::new(AtomicU64::new(0)),
            columns_cache: Arc::new(tokio::sync::Mutex::new(None)),
        };
        let _ = db.probe_optional_columns(true).await;
        Ok(db)
    }

    pub async fn probe_optional_columns(&self, force: bool) -> Result<OptionalColumns> {
        if !force {
            let packed = self.columns_fast.load(Ordering::Acquire);
            if (packed & 0x100) != 0 {
                let cols = OptionalColumns::from_bits((packed & 0xFF) as u8);
                let probed_ms = packed >> 9;
                let now_ms = self.columns_epoch.elapsed().as_millis() as u64;
                let ttl_ms = if cols.all_present() { 300_000 } else { 30_000 };
                if now_ms.saturating_sub(probed_ms) < ttl_ms {
                    return Ok(cols);
                }
            }
        }
        let mut guard = self.columns_cache.lock().await;
        if !force {
            if let Some((probed_at, cols)) = *guard {
                let ttl = if cols.all_present() {
                    Duration::from_secs(300)
                } else {
                    Duration::from_secs(30)
                };
                if probed_at.elapsed() < ttl {
                    return Ok(cols);
                }
            }
        }
        let row = sqlx::query(OPTIONAL_COLUMNS_SQL)
            .fetch_one(&self.pool)
            .await?;
        let tables_present = row.try_get::<bool, _>("accounts_present")?
            && row.try_get::<bool, _>("errors_present")?
            && row.try_get::<bool, _>("usage_present")?;
        let cols = OptionalColumns {
            account_parent_id: row.try_get("account_parent_id")?,
            account_session_window_end: row.try_get("account_session_window_end")?,
            account_session_window_status: row.try_get("account_session_window_status")?,
            error_upstream_errors: row.try_get("error_upstream_errors")?,
            error_body: row.try_get("error_body")?,
            error_upstream_detail: row.try_get("error_upstream_detail")?,
            usage_video_count: row.try_get("usage_video_count")?,
            usage_video_duration: row.try_get("usage_video_duration")?,
        };
        if tables_present {
            let now = Instant::now();
            *guard = Some((now, cols));
            let now_ms = now.duration_since(self.columns_epoch).as_millis() as u64;
            let packed = (now_ms << 9) | 0x100 | u64::from(cols.to_bits());
            self.columns_fast.store(packed, Ordering::Release);
        } else {
            *guard = None;
            self.columns_fast.store(0, Ordering::Release);
        }
        Ok(cols)
    }

    pub async fn all_with_columns<F>(&self, build_sql: F, params: &[Param]) -> Result<Vec<Value>>
    where
        F: Fn(OptionalColumns) -> Cow<'static, str>,
    {
        let cols = self.probe_optional_columns(false).await?;
        let sql = build_sql(cols);
        match bind(&sql, params).fetch_all(&self.pool).await {
            Ok(rows) => rows.into_iter().map(row_value).collect(),
            Err(err) if is_undefined_column(&err) => {
                let refreshed = self.probe_optional_columns(true).await?;
                let retry_sql = build_sql(refreshed);
                bind(&retry_sql, params)
                    .fetch_all(&self.pool)
                    .await?
                    .into_iter()
                    .map(row_value)
                    .collect()
            }
            Err(err) => Err(err.into()),
        }
    }

    pub async fn one_with_columns<F>(&self, build_sql: F, params: &[Param]) -> Result<Option<Value>>
    where
        F: Fn(OptionalColumns) -> Cow<'static, str>,
    {
        let cols = self.probe_optional_columns(false).await?;
        let sql = build_sql(cols);
        match bind(&sql, params).fetch_optional(&self.pool).await {
            Ok(row) => row.map(row_value).transpose(),
            Err(err) if is_undefined_column(&err) => {
                let refreshed = self.probe_optional_columns(true).await?;
                let retry_sql = build_sql(refreshed);
                bind(&retry_sql, params)
                    .fetch_optional(&self.pool)
                    .await?
                    .map(row_value)
                    .transpose()
            }
            Err(err) => Err(err.into()),
        }
    }

    pub async fn all(&self, sql: &str, params: &[Param]) -> Result<Vec<Value>> {
        bind(sql, params)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(row_value)
            .collect()
    }
    pub async fn one(&self, sql: &str, params: &[Param]) -> Result<Option<Value>> {
        bind(sql, params)
            .fetch_optional(&self.pool)
            .await?
            .map(row_value)
            .transpose()
    }
    pub async fn execute(&self, sql: &str, params: &[Param]) -> Result<u64> {
        Ok(bind(sql, params).execute(&self.pool).await?.rows_affected())
    }
    pub async fn close(&self) {
        loop {
            self.pool.close().await;
            if self.pool.size() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
