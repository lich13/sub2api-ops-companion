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

#[derive(Clone)]
pub struct Database {
    pub pool: PgPool,
    read_only: bool,
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
                // psycopg returns Decimal; Python's default=str is part of hashes.
                "NUMERIC" => {
                    let raw = row.try_get_raw(i)?;
                    let mut amount = row.try_get::<sqlx::types::BigDecimal, _>(i)?;
                    if raw.format() == PgValueFormat::Binary {
                        // SQLx reconstructs a base-10000 value but discards the
                        // display scale. Preserve PostgreSQL's decimal places,
                        // including zero, for Python-compatible values and hashes.
                        let bytes = raw.as_bytes().map_err(|_| Error::internal())?;
                        let scale = bytes.get(6..8).ok_or_else(Error::internal)?;
                        amount =
                            amount.with_scale(i64::from(u16::from_be_bytes([scale[0], scale[1]])));
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
    pub async fn open(url: &str, read_only: bool) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(8)
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    sqlx::query("SET TIME ZONE 'UTC'")
                        .execute(&mut *connection)
                        .await?;
                    if read_only {
                        sqlx::query("SET default_transaction_read_only = on")
                            .execute(&mut *connection)
                            .await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Self { pool, read_only })
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
        if self.read_only {
            return Err(Error::read_only());
        }
        Ok(bind(sql, params).execute(&self.pool).await?.rows_affected())
    }
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
