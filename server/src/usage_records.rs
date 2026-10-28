use crate::{
    compat::{config_revision, datetime, integer, pick, string, text, truthy},
    db::{bind, row_value, Database, Param},
    error::{Error, Result},
    sql,
};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Offset, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::types::BigDecimal;
use std::str::FromStr;

const FIELDS:&str="id user_id api_key_id account_id group_id request_id upstream_request_id model requested_model upstream_model upstream_response_model model_mapping_chain upstream_model_mismatch reasoning_effort requested_reasoning_effort request_type stream openai_ws_mode native_compaction_v2 input_tokens output_tokens cache_creation_tokens cache_read_tokens cache_creation_5m_tokens cache_creation_1h_tokens input_cost output_cost cache_creation_cost cache_read_cost total_cost actual_cost account_stats_cost account_rate_multiplier rate_multiplier first_token_ms duration_ms created_at user_agent ip_address inbound_endpoint upstream_endpoint billing_type billing_mode service_tier image_count image_output_tokens image_output_cost image_input_tokens image_input_cost video_count video_duration_seconds video_resolution user_name user_email api_key_name account_name group_name";
const MONEY:&str="input_cost output_cost cache_creation_cost cache_read_cost total_cost actual_cost account_stats_cost account_rate_multiplier rate_multiplier image_output_cost image_input_cost";
pub fn timestamp(value: &str) -> Result<DateTime<Utc>> {
    let invalid = || Error::new(422, "时间必须包含时区");
    let parsed =
        DateTime::parse_from_rfc3339(&value.replacen(' ', "T", 1)).map_err(|_| invalid())?;
    let utc = parsed.with_timezone(&Utc);
    if (1..=9999).contains(&parsed.year()) && (1..=9999).contains(&utc.year()) {
        Ok(utc)
    } else {
        Err(invalid())
    }
}
pub fn date_window(start: &str, end: &str) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let fail = || Error::new(422, "请选择有效的北京时间日期范围");
    let parse = |s: &str| {
        let bytes = s.as_bytes();
        if bytes.len() != 10
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || !bytes
                .iter()
                .enumerate()
                .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
        {
            return Err(fail());
        }
        NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| fail())
            .and_then(|value| {
                if (1..=9999).contains(&value.year()) {
                    Ok(value)
                } else {
                    Err(fail())
                }
            })
    };
    let start = parse(start)?;
    let end = parse(end)?;
    if start > end {
        return Err(fail());
    }
    let end = end
        .succ_opt()
        .filter(|d| d.year() <= 9999)
        .ok_or_else(fail)?;
    let zone = chrono_tz::Asia::Shanghai;
    let convert = |d: NaiveDate| {
        let local = d.and_hms_opt(0, 0, 0).unwrap();
        zone.from_local_datetime(&local)
            .earliest()
            .map(|v| v.with_timezone(&Utc))
            .or_else(|| {
                // Python's zoneinfo attaches fold=0. During a clock gap this
                // uses the offset immediately before the transition.
                let (_, offset) = chrono_tz::GapInfo::new(&local, &zone)?.begin?;
                offset
                    .fix()
                    .from_local_datetime(&local)
                    .single()
                    .map(|v| v.with_timezone(&Utc))
            })
            .filter(|v| (1..=9999).contains(&v.year()))
            .ok_or_else(fail)
    };
    Ok((convert(start)?, convert(end)?))
}
/// Python Decimal's string form retains trailing decimal places and switches
/// to an exponent only below 1e-6 or for a positive exponent.
pub fn decimal_text(value: &BigDecimal, force_fixed: bool) -> String {
    let (coefficient, scale) = value.as_bigint_and_exponent();
    let raw = coefficient.to_string();
    let negative = raw.starts_with('-');
    let digits = raw.trim_start_matches('-');
    let sign = if negative { "-" } else { "" };
    let adjusted = digits.len() as i64 - 1 - scale;
    if force_fixed || scale >= 0 && adjusted >= -6 {
        if scale <= 0 {
            return format!("{sign}{digits}{}", "0".repeat((-scale) as usize));
        }
        if digits.len() as i64 > scale {
            let split = digits.len() - scale as usize;
            format!("{sign}{}.{}", &digits[..split], &digits[split..])
        } else {
            format!(
                "{sign}0.{}{digits}",
                "0".repeat((scale - digits.len() as i64) as usize)
            )
        }
    } else {
        format!(
            "{sign}{}{}E{}{adjusted}",
            &digits[..1],
            if digits.len() > 1 {
                format!(".{}", &digits[1..])
            } else {
                String::new()
            },
            if adjusted >= 0 { "+" } else { "" }
        )
    }
}
pub fn project(row: &Value) -> Result<Value> {
    let mut result = pick(row, &FIELDS.split_whitespace().collect::<Vec<_>>());
    result["native_compaction_v2"] = truthy(&row["native_compaction_v2"]).into();
    for field in MONEY.split_whitespace() {
        if !result[field].is_null() {
            result[field] = text(&result[field]).into();
        }
    }
    if !result["created_at"].is_null() {
        result["created_at"] = datetime(timestamp(string(&result["created_at"]))?).into();
    }
    let base = if row["account_stats_cost"].is_null() {
        &row["total_cost"]
    } else {
        &row["account_stats_cost"]
    };
    result["account_cost"] = Value::Null;
    if !base.is_null() {
        let amount = BigDecimal::from_str(&text(base)).map_err(|_| Error::internal())?;
        let rate = if row["account_rate_multiplier"].is_null() {
            BigDecimal::from(1)
        } else {
            BigDecimal::from_str(&text(&row["account_rate_multiplier"]))
                .map_err(|_| Error::internal())?
        };
        result["account_cost"] = decimal_text(&(amount * rate), false).into();
    }
    let native = integer(&row["request_type"]);
    let kind = match native {
        Some(1) => "sync",
        Some(2) => "stream",
        Some(3) => "ws_v2",
        _ => {
            if truthy(&row["openai_ws_mode"]) {
                "ws_v2"
            } else if truthy(&row["stream"]) {
                "stream"
            } else {
                "sync"
            }
        }
    };
    result["request_type"] = kind.into();
    result["native_request_type"] = match native {
        Some(4) => "cyber",
        Some(5) => "live",
        _ => kind,
    }
    .into();
    let requested = if truthy(&row["requested_model"]) {
        row["requested_model"].clone()
    } else if truthy(&row["model"]) {
        row["model"].clone()
    } else {
        Value::Null
    };
    let forwarded = if truthy(&row["upstream_model"]) {
        &row["upstream_model"]
    } else {
        &requested
    };
    let response = &row["upstream_response_model"];
    result["upstream_model_mismatch"] = (truthy(&row["upstream_model_mismatch"])
        || truthy(response) && truthy(forwarded) && response != forwarded)
        .into();
    result["requested_model"] = requested;
    Ok(result)
}
fn escaped(value: &str) -> String {
    format!(
        "%{}%",
        value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}
async fn usage_selection(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<&'static str> {
    static WITH_COMPACTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
            "{}, COALESCE(u.native_compaction_v2, false) AS native_compaction_v2",
            sql::USAGE_SELECT
        )
    });
    static WITHOUT_COMPACTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!("{}, false AS native_compaction_v2", sql::USAGE_SELECT)
    });
    // Resolve the same relation as the page/detail query, including its search_path.
    // Check on each read so an upstream migration takes effect without a restart.
    let has_compaction: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = pg_catalog.to_regclass('usage_logs') AND attname = 'native_compaction_v2' AND attnum > 0 AND NOT attisdropped)",
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(if has_compaction {
        WITH_COMPACTION.as_str()
    } else {
        WITHOUT_COMPACTION.as_str()
    })
}
fn decode_cursor(value: &str) -> Result<Value> {
    if value.len() > 2048 {
        return Err(Error::invalid());
    }
    let bytes = URL_SAFE.decode(value).map_err(|_| Error::invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| Error::invalid())
}
fn cursor(fields: &[(&str, Value)]) -> String {
    // The old cursor uses json.dumps insertion order, unlike the sorted objects
    // used by version hashes. Keep its bytes stable across service upgrades.
    let entries = fields
        .iter()
        .map(|(name, value)| {
            format!(
                "{}: {}",
                crate::compat::py_json(&json!(name), true, false),
                crate::compat::py_json(value, true, false)
            )
        })
        .collect::<Vec<_>>();
    URL_SAFE.encode(format!("{{{}}}", entries.join(", ")))
}
#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Filter {
    pub from_at: Option<String>,
    pub to_at: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub include_summary: bool,
    pub account_id: Option<i64>,
    pub platform: Option<String>,
    pub api_key_id: Option<i64>,
    pub user_id: Option<i64>,
    pub model: Option<String>,
    pub request_type: Option<String>,
    pub mismatch_only: bool,
    pub cursor: Option<String>,
    pub after_id: Option<i64>,
    pub limit: Option<usize>,
}
#[derive(Clone)]
pub struct UsageRecords(pub Database);
impl UsageRecords {
    async fn begin(&self, repeatable: bool) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        let mut transaction = self.0.pool.begin().await?;
        sqlx::query(if repeatable {
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY"
        } else {
            "SET TRANSACTION READ ONLY"
        })
        .execute(&mut *transaction)
        .await?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *transaction)
            .await?;
        Ok(transaction)
    }
    pub async fn list(&self, f: &Filter) -> Result<Value> {
        self.list_at(f, Utc::now()).await
    }
    pub async fn list_at(&self, f: &Filter, at: DateTime<Utc>) -> Result<Value> {
        let limit = f.limit.unwrap_or(50);
        if !(1..=100).contains(&limit)
            || [f.account_id, f.api_key_id, f.user_id]
                .into_iter()
                .flatten()
                .any(|v| v < 1)
            || f.after_id.is_some_and(|v| v < 0)
        {
            return Err(Error::new(422, "分页或筛选参数无效"));
        }
        if f.platform.as_ref().is_some_and(|v| v.len() > 80) {
            return Err(Error::new(422, "平台筛选无效"));
        }
        let has_cursor = f.cursor.as_ref().is_some_and(|v| !v.is_empty());
        if has_cursor && f.after_id.is_some() {
            return Err(Error::new(422, "分页和新增检查不能同时使用"));
        }
        let dates = f.start_date.is_some() || f.end_date.is_some();
        let window = if dates {
            if f.start_date.as_ref().is_none_or(|v| v.is_empty())
                || f.end_date.as_ref().is_none_or(|v| v.is_empty())
                || f.from_at.is_some()
                || f.to_at.is_some()
            {
                return Err(Error::new(422, "日期必须成对提供，不能与时间参数混用"));
            }
            Some(date_window(
                f.start_date.as_ref().unwrap(),
                f.end_date.as_ref().unwrap(),
            )?)
        } else {
            None
        };
        let mut criteria = json!([
            f.from_at,
            f.to_at,
            f.account_id,
            f.api_key_id,
            f.model,
            f.request_type,
            f.mismatch_only
        ]);
        if let Some(user) = f.user_id {
            criteria.as_array_mut().unwrap().push(user.into());
        }
        if dates {
            criteria
                .as_array_mut()
                .unwrap()
                .extend([json!(f.start_date), json!(f.end_date)]);
        }
        if let Some(platform) = f.platform.as_ref().filter(|v| !v.is_empty()) {
            criteria
                .as_array_mut()
                .unwrap()
                .push(json!({"platform": platform}));
        }
        let signature = config_revision(&criteria);
        let saved = if has_cursor {
            let validate = || -> Result<Value> {
                let saved = decode_cursor(f.cursor.as_ref().unwrap())?;
                if saved["signature"] != signature
                    || integer(&saved["id"]).is_none_or(|id| id < 1)
                    || integer(&saved["watermark"]).is_none_or(|id| id < 0)
                {
                    return Err(Error::invalid());
                }
                for key in ["at", "from_at", "to_at"] {
                    timestamp(string(&saved[key]))?;
                }
                Ok(saved)
            };
            Some(validate().map_err(|_| Error::new(422, "分页已失效，请刷新记录"))?)
        } else {
            None
        };
        let (start, end) = if let Some(saved) = &saved {
            (
                timestamp(string(&saved["from_at"]))?,
                timestamp(string(&saved["to_at"]))?,
            )
        } else if let Some(window) = window {
            window
        } else {
            (
                f.from_at
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .map(|v| timestamp(v))
                    .transpose()?
                    .unwrap_or(at - Duration::hours(24)),
                f.to_at
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .map(|v| timestamp(v))
                    .transpose()?
                    .unwrap_or(at),
            )
        };
        if start >= end {
            return Err(Error::new(422, "开始时间必须早于结束时间"));
        }
        let mut clauses = vec![
            "u.created_at >= $1".to_owned(),
            format!("u.created_at {} $2", if dates { "<" } else { "<=" }),
        ];
        let mut params = vec![Param::from(start), Param::from(end)];
        for (field, value) in [
            ("account_id", f.account_id),
            ("api_key_id", f.api_key_id),
            ("user_id", f.user_id),
        ] {
            if let Some(value) = value {
                params.push(value.into());
                clauses.push(format!("u.{field} = ${}", params.len()));
            }
        }
        if let Some(platform) = f.platform.as_ref().filter(|v| !v.is_empty()) {
            params.push(platform.clone().into());
            clauses.push(format!("EXISTS (SELECT 1 FROM accounts platform_account WHERE platform_account.id=u.account_id AND platform_account.platform=${})", params.len()));
        }
        if let Some(model) = f.model.as_ref().filter(|s| !s.is_empty()) {
            if model.chars().count() > 200 {
                return Err(Error::new(422, "模型筛选过长"));
            }
            params.push(escaped(model).into());
            clauses.push(format!(
                "({})",
                [
                    "model",
                    "requested_model",
                    "upstream_model",
                    "upstream_response_model",
                    "model_mapping_chain"
                ]
                .iter()
                .map(|field| format!("u.{field} ILIKE ${}", params.len()))
                .collect::<Vec<_>>()
                .join(" OR ")
            ));
        }
        if let Some(kind) = f.request_type.as_ref().filter(|s| !s.is_empty()) {
            let code = match kind.as_str() {
                "sync" => 1i64,
                "stream" => 2,
                "ws_v2" => 3,
                _ => return Err(Error::new(422, "请求类型无效")),
            };
            params.push(code.into());
            clauses.push(format!("({}) = ${}", sql::USAGE_REQUEST_TYPE, params.len()));
        }
        if f.mismatch_only {
            clauses.push(sql::USAGE_MISMATCH.to_owned());
        }
        let mut tx = self.begin(true).await?;
        let live_watermark: i64 = sqlx::query_scalar("SELECT coalesce(max(id),0) FROM usage_logs")
            .fetch_one(&mut *tx)
            .await?;
        let watermark = saved
            .as_ref()
            .and_then(|s| integer(&s["watermark"]))
            .unwrap_or(live_watermark);
        params.push(watermark.into());
        clauses.push(format!("u.id <= ${}", params.len()));
        if let Some(after) = f.after_id {
            params.push(after.into());
            clauses.push(format!("u.id > ${}", params.len()));
            let row = bind(
                &format!(
                    "SELECT count(*) AS count FROM usage_logs u WHERE {}",
                    clauses.join(" AND ")
                ),
                &params,
            )
            .fetch_one(&mut *tx)
            .await?;
            let count = row_value(row)?["count"].clone();
            tx.commit().await?;
            return Ok(json!({"new_count":count,"latest_id":live_watermark}));
        }
        let summary = if f.include_summary && saved.is_none() {
            let row=bind(&format!("SELECT coalesce(sum(u.actual_cost),0) AS actual_cost FROM usage_logs u WHERE {}",clauses.join(" AND ")),&params).fetch_one(&mut *tx).await?;
            let value = row_value(row)?;
            let amount = BigDecimal::from_str(&text(&value["actual_cost"]))
                .map_err(|_| Error::internal())?;
            Some(json!({"actual_cost":decimal_text(&amount,true)}))
        } else {
            None
        };
        if let Some(saved) = &saved {
            params.push(timestamp(string(&saved["at"]))?.into());
            params.push(integer(&saved["id"]).unwrap().into());
            clauses.push(format!(
                "(u.created_at,u.id) < (${},${})",
                params.len() - 1,
                params.len()
            ));
        }
        params.push(((limit + 1) as i64).into());
        let selection = usage_selection(&mut tx).await?;
        let rows=bind(&format!("SELECT {} FROM usage_logs u {} WHERE {} ORDER BY u.created_at DESC,u.id DESC LIMIT ${}",selection,sql::USAGE_JOINS,clauses.join(" AND "),params.len()),&params).fetch_all(&mut *tx).await?.into_iter().map(row_value).collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        let items = rows
            .iter()
            .take(limit)
            .map(project)
            .collect::<Result<Vec<_>>>()?;
        let next = if rows.len() > limit {
            let last = items.last().unwrap();
            Some(cursor(&[
                ("at", last["created_at"].clone()),
                ("id", last["id"].clone()),
                ("watermark", json!(watermark)),
                ("from_at", json!(datetime(start))),
                ("to_at", json!(datetime(end))),
                ("signature", json!(signature)),
            ]))
        } else {
            None
        };
        let mut page = json!({"items":items,"next_cursor":next,"latest_id":watermark,"observed_at":datetime(at)});
        if let Some(summary) = summary {
            page["summary"] = summary;
        }
        Ok(page)
    }
    pub async fn options(
        &self,
        kind: &str,
        q: Option<&str>,
        user: Option<i64>,
        saved: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        if !matches!(kind, "users" | "api_keys") || !(1..=100).contains(&limit) {
            return Err(Error::new(422, "目录或分页参数无效"));
        }
        if user.is_some_and(|v| kind != "api_keys" || v < 1) {
            return Err(Error::new(422, "用户筛选无效"));
        }
        let query = q.unwrap_or("").trim();
        if query.chars().count() > 200 {
            return Err(Error::new(422, "搜索内容过长"));
        }
        let signature = config_revision(&json!([kind, query, user]));
        let after = if let Some(saved) = saved.filter(|s| !s.is_empty()) {
            let saved =
                decode_cursor(saved).map_err(|_| Error::new(422, "目录分页已失效，请重新搜索"))?;
            if saved["signature"] != signature || integer(&saved["id"]).is_none_or(|id| id < 1) {
                return Err(Error::new(422, "目录分页已失效，请重新搜索"));
            }
            integer(&saved["id"]).unwrap()
        } else {
            0
        };
        let mut params = vec![Param::from(after)];
        let mut clauses = vec!["e.id > $1".to_owned(), "e.deleted_at IS NULL".to_owned()];
        let (selection, source, text_fields, fields) = if kind == "users" {
            (
                "e.id,e.username AS name,e.email,e.status,(e.deleted_at IS NOT NULL) AS deleted",
                "users e",
                vec!["e.username", "e.email"],
                vec!["id", "name", "email", "status", "deleted"],
            )
        } else {
            if let Some(user) = user {
                params.push(user.into());
                clauses.push(format!("e.user_id = ${}", params.len()));
            }
            ("e.id,e.name,e.user_id,p.username AS user_name,p.email AS user_email,e.status,(e.deleted_at IS NOT NULL) AS deleted","api_keys e JOIN users p ON p.id=e.user_id AND p.deleted_at IS NULL",vec!["e.name"],vec!["id","name","user_id","user_name","user_email","status","deleted"])
        };
        if !query.is_empty() {
            params.push(escaped(query).into());
            let mut matches = text_fields
                .iter()
                .map(|field| format!("{field} ILIKE ${}", params.len()))
                .collect::<Vec<_>>();
            let digits = query.strip_prefix('#').unwrap_or(query);
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(id) = digits.parse::<i64>() {
                    if id > 0 {
                        params.push(id.into());
                        matches.push(format!("e.id = ${}", params.len()));
                    }
                }
            }
            clauses.push(format!("({})", matches.join(" OR ")));
        }
        params.push(((limit + 1) as i64).into());
        let mut tx = self.begin(false).await?;
        let rows = bind(
            &format!(
                "SELECT {selection} FROM {source} WHERE {} ORDER BY e.id ASC LIMIT ${}",
                clauses.join(" AND "),
                params.len()
            ),
            &params,
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(row_value)
        .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        let items: Vec<_> = rows
            .iter()
            .take(limit)
            .map(|r| {
                let mut v = pick(r, &fields);
                v["deleted"] = truthy(&r["deleted"]).into();
                v
            })
            .collect();
        let next = if rows.len() > limit {
            Some(cursor(&[
                ("id", items.last().unwrap()["id"].clone()),
                ("signature", json!(signature)),
            ]))
        } else {
            None
        };
        Ok(json!({"items":items,"next_cursor":next}))
    }
    pub async fn detail(&self, id: i64) -> Result<Value> {
        if id < 1 {
            return Err(Error::new(422, "记录编号无效"));
        }
        let mut tx = self.begin(false).await?;
        let selection = usage_selection(&mut tx).await?;
        let row = bind(
            &format!(
                "SELECT {} FROM usage_logs u {} WHERE u.id=$1",
                selection,
                sql::USAGE_JOINS
            ),
            &[id.into()],
        )
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let row = row.ok_or_else(|| Error::new(404, "记录不存在"))?;
        project(&row_value(row)?)
    }
}
