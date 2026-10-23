use crate::{
    compat::{
        account_version, datetime, float, future, integer, operation_versions, parse_datetime,
        pick, string, truthy,
    },
    db::{Database, Param},
    error::{Error, Result},
    oauth_state::OAuthState,
    quota,
    security::{clean, safe_error, safe_error_body},
    sql,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn settings_capabilities(row: &Value) -> Value {
    let extra = &row["extra"];
    let inherited = truthy(&row["parent_account_id"]) || truthy(&extra["parent_account_id"]);
    let passthrough = row["platform"] == "openai"
        && ["openai_passthrough", "openai_oauth_passthrough"]
            .into_iter()
            .any(|k| extra[k] == true || extra[k] == "true");
    let reason = if inherited {
        "继承母账号凭据，不能独立修改模型配置"
    } else if passthrough {
        "透传模式会绕过模型配置"
    } else {
        ""
    };
    let mut sync = matches!(
        string(&row["platform"]),
        "openai"
            | "grok"
            | "anthropic"
            | "gemini"
            | "antigravity"
            | "kimi"
            | "zhipu"
            | "deepseek"
            | "opencode"
            | "opencodego"
    );
    if row["platform"] != "antigravity" && !matches!(string(&row["type"]), "oauth" | "apikey")
        || row["platform"] == "gemini"
            && row["type"] == "oauth"
            && truthy(&row["model_sync_project"])
    {
        sync = false;
    }
    json!({"priority":{"editable":true,"reason":""},"concurrency":{"editable":true,"reason":""},"load_factor":{"editable":true,"reason":""},
        "model_config":{"editable":reason.is_empty(),"reason":reason},"proxy":{"editable":!inherited,"reason":if inherited {"代理继承母账号，不能独立修改"} else {""}},
        "sync_models":{"editable":sync && !inherited,"reason":if inherited {"请在母账号同步模型"} else if sync {""} else {"该账号不支持上游模型同步"}}})
}
pub fn recoverable(row: &Value, at: DateTime<Utc>) -> bool {
    row["status"] == "error"
        || [
            "rate_limit_reset_at",
            "overload_until",
            "temp_unschedulable_until",
        ]
        .into_iter()
        .any(|k| future(&row[k], at))
        || row["extra"]["model_rate_limits"]
            .as_object()
            .is_some_and(|v| {
                v.values()
                    .any(|v| v.is_object() && future(&v["rate_limit_reset_at"], at))
            })
}

pub fn account_dto(row: &Value, at: DateTime<Utc>, quota_result: &Value) -> Value {
    let mut value = pick(
        row,
        &[
            "id",
            "name",
            "platform",
            "type",
            "status",
            "schedulable",
            "updated_at",
            "group_ids",
            "priority",
            "concurrency",
            "load_factor",
            "last_success_at",
            "last_called_at",
            "last_error_at",
            "last_error_id",
            "last_error_code",
            "last_error_status",
        ],
    );
    if row["type"] == "apikey" {
        if let Some(parent) = integer(&row["parent_account_id"])
            .or_else(|| integer(&row["extra"]["parent_account_id"]))
            .filter(|id| *id > 0)
        {
            value["parent_account_id"] = parent.into();
        }
    }
    value["name"] = clean(&row["name"], 160).into();
    value["priority"] = integer(&value["priority"]).unwrap_or(0).into();
    if value["load_factor"].is_string() {
        value["load_factor"] = json!(float(&value["load_factor"]));
    }
    for key in [
        "updated_at",
        "last_success_at",
        "last_called_at",
        "last_error_at",
    ] {
        if let Some(time) = parse_datetime(&value[key]) {
            value[key] = datetime(time).into();
        }
    }
    value["settings_capabilities"] = settings_capabilities(row);
    value["group_ids"] = row["group_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into();
    value["usage_windows"] = quota::usage_windows(row, at, quota_result).into();
    value["recovery_selectable"] = json!(
        row["platform"] == "openai"
            && row["type"] == "oauth"
            && !truthy(&row["parent_account_id"])
            && !truthy(&row["extra"]["parent_account_id"])
    );
    value["recoverable"] = recoverable(row, at).into();
    value["error_message"] = safe_error(
        if truthy(&row["last_error_message"]) {
            &row["last_error_message"]
        } else {
            &row["error_message"]
        },
        600,
    )
    .into();
    let mut reasons = vec![];
    if row["status"] != "active" {
        reasons.push(json!({"code":"status","label":if row["status"] == "error" {"认证异常"} else {"账号非 active"}}));
    }
    if row["schedulable"] != true {
        reasons.push(json!({"code":"disabled","label":"调度已关闭"}));
    }
    for (key, label) in [
        ("temp_unschedulable_until", "冷却中"),
        ("rate_limit_reset_at", "限流中"),
        ("overload_until", "过载保护"),
    ] {
        if future(&row[key], at) {
            reasons.push(
                json!({"code":key,"label":label,"until":parse_datetime(&row[key]).map(datetime)}),
            );
        }
    }
    if truthy(&row["expires_at"])
        && truthy(&row["auto_pause_on_expired"])
        && !future(&row["expires_at"], at)
    {
        reasons.push(json!({"code":"expired","label":"到期暂停"}));
    }
    if truthy(&row["needs_reauth"]) {
        reasons.push(json!({"code":"reauth","label":"需要重新认证"}));
    }
    value["available"] = reasons.is_empty().into();
    value["blockers"] = reasons.into();
    value["success_after_error"] = parse_datetime(&row["last_success_at"])
        .zip(parse_datetime(&row["last_error_at"]))
        .is_some_and(|(s, e)| s > e)
        .into();
    value["version"] = account_version(row).into();
    value["operation_versions"] = operation_versions(row);
    value
}

pub fn group_dtos(rows: &[Value], memberships: &BTreeMap<i64, BTreeSet<i64>>) -> Vec<Value> {
    let fields = [
        "log_id",
        "account_id",
        "account_name",
        "model",
        "upstream_model",
        "called_at",
    ];
    let mut groups: Vec<Value> = vec![];
    let mut positions = BTreeMap::new();
    for row in rows {
        let id = integer(&row["id"]).unwrap_or(0);
        let position = *positions.entry(id).or_insert_with(|| {
            let position = groups.len();
            let mut group = pick(row, &["id", "name", "platform", "sort_order"]);
            for field in fields {
                group[field] = Value::Null;
            }
            group["recent_accounts"] = json!([]);
            groups.push(group);
            position
        });
        if integer(&row["account_id"])
            .is_some_and(|aid| memberships.get(&aid).is_some_and(|v| v.contains(&id)))
        {
            groups[position]["recent_accounts"]
                .as_array_mut()
                .unwrap()
                .push(pick(row, &fields));
        }
    }
    for group in &mut groups {
        let rows = group["recent_accounts"].as_array_mut().unwrap();
        rows.sort_by_key(|v| {
            std::cmp::Reverse((
                parse_datetime(&v["called_at"]),
                integer(&v["log_id"]).unwrap_or(0),
            ))
        });
        let mut seen = BTreeSet::new();
        rows.retain(|v| seen.insert(integer(&v["account_id"])));
        rows.truncate(3);
        for call in rows.iter_mut() {
            for field in ["account_name", "model", "upstream_model"] {
                call[field] = clean(&call[field], 160).into();
            }
            if let Some(time) = parse_datetime(&call["called_at"]) {
                call["called_at"] = datetime(time).into();
            }
        }
        let first = rows.first().cloned();
        if let Some(first) = first {
            group
                .as_object_mut()
                .unwrap()
                .extend(first.as_object().unwrap().clone());
        }
        for field in [
            "name",
            "account_name",
            "model",
            "upstream_model",
            "upstream_response_model",
        ] {
            group[field] = clean(&group[field], 160).into();
        }
    }
    groups
}
pub fn error_dto(row: &Value, detail: bool) -> Value {
    let mut value = pick(
        row,
        &[
            "id",
            "account_id",
            "group_id",
            "created_at",
            "platform",
            "model",
            "requested_model",
            "upstream_model",
            "status_code",
            "upstream_status_code",
            "provider_error_code",
            "error_type",
            "request_id",
            "resolved",
            "account_name",
            "group_name",
            "error_owner",
            "error_phase",
            "error_source",
        ],
    );
    for (key, item) in value.as_object_mut().unwrap() {
        if key == "created_at" {
            if let Some(time) = parse_datetime(item) {
                *item = datetime(time).into();
            }
        } else if item.is_string() {
            *item = clean(item, 300).into();
        }
    }
    value["message"] = safe_error(
        if truthy(&row["upstream_error_message"]) {
            &row["upstream_error_message"]
        } else {
            &row["error_message"]
        },
        600,
    )
    .into();
    if detail {
        let content = safe_error_body(if truthy(&row["upstream_error_detail"]) {
            &row["upstream_error_detail"]
        } else {
            &row["error_body"]
        });
        value["content_limited"] = content.contains("...").into();
        value["content"] = content.into();
    }
    value
}
pub fn latest_completed_result(result: &Value, meta: &Value) -> Value {
    let error = string(&meta["last_error_code"]).trim();
    let error_at = parse_datetime(&meta["last_error_at"]);
    let success = parse_datetime(&meta["last_success_at"])
        .into_iter()
        .chain(parse_datetime(&result["queried_at"]))
        .max();
    if !error.is_empty() && error_at.is_some_and(|e| success.is_none_or(|s| e > s)) {
        json!({"success":false,"queried_at":error_at.map(datetime),"error_code":error})
    } else if result.is_object() {
        result.clone()
    } else {
        Value::Null
    }
}

#[derive(Clone)]
pub struct Queries {
    pub db: Database,
    pub oauth: OAuthState,
}
impl Queries {
    pub async fn accounts(&self) -> Result<Vec<Value>> {
        self.db
            .all(&sql::DESKTOP_ACCOUNT_SQL.replace("{filter}", ""), &[])
            .await
    }
    pub async fn account(&self, id: i64) -> Result<Value> {
        self.db
            .one(
                &sql::DESKTOP_ACCOUNT_SQL.replace("{filter}", "AND a.id=$1"),
                &[id.into()],
            )
            .await?
            .ok_or_else(|| Error::new(404, "账号不存在"))
    }
    pub async fn groups(&self, accounts: &[Value]) -> Result<Vec<Value>> {
        let memberships = accounts
            .iter()
            .filter_map(|a| {
                integer(&a["id"]).map(|id| {
                    (
                        id,
                        a["group_ids"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(integer)
                            .collect(),
                    )
                })
            })
            .collect();
        Ok(group_dtos(
            &self.db.all(sql::DESKTOP_GROUP_SQL, &[]).await?,
            &memberships,
        ))
    }
    fn error_filter(category: Option<&str>) -> Result<&'static str> {
        match category {
            None => Ok(sql::ERRORS_ERROR_WHERE),
            Some("degradation") => Ok(sql::ERRORS_DEGRADATION_WHERE),
            Some("other") => Ok(sql::ERRORS_OTHER_WHERE),
            _ => Err(Error::new(422, "未知错误分类")),
        }
    }
    pub async fn errors(
        &self,
        account: Option<i64>,
        before: Option<i64>,
        limit: usize,
        category: Option<&str>,
    ) -> Result<Value> {
        let filter = Self::error_filter(category)?;
        let query = format!("SELECT {} FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id LEFT JOIN groups g ON g.id=e.group_id WHERE {filter} AND ($1::bigint IS NULL OR e.account_id=$1) AND ($2::bigint IS NULL OR e.id<$2) ORDER BY e.id DESC LIMIT $3",sql::DESKTOP_ERROR_FIELDS);
        let rows = self
            .db
            .all(
                &query,
                &[
                    Param::Int(account),
                    Param::Int(before),
                    ((limit + 1) as i64).into(),
                ],
            )
            .await?;
        Ok(
            json!({"items":rows.iter().take(limit).map(|v|error_dto(v,false)).collect::<Vec<_>>(),"next_cursor":if rows.len() > limit {rows[limit-1]["id"].clone()} else {Value::Null}}),
        )
    }
    pub async fn error_detail(&self, id: i64, category: Option<&str>) -> Result<Value> {
        let filter = Self::error_filter(category)?;
        let query = format!("SELECT {},e.error_body,e.upstream_error_detail FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id LEFT JOIN groups g ON g.id=e.group_id WHERE {filter} AND e.id=$1",sql::DESKTOP_ERROR_FIELDS);
        let row = self
            .db
            .one(&query, &[id.into()])
            .await?
            .ok_or_else(|| Error::new(404, "错误记录不存在或已过保留期"))?;
        Ok(error_dto(&row, true))
    }
    pub async fn recoveries(&self, before: Option<i64>, limit: usize) -> Result<Value> {
        let state = self.oauth.snapshot()?;
        let mut rows: Vec<Value> = state["recovery_history"]
            .as_object()
            .into_iter()
            .flat_map(|v| v.values())
            .filter(|v| before.is_none_or(|b| integer(&v["id"]).unwrap_or(0) < b))
            .cloned()
            .collect();
        rows.sort_by_key(|v| std::cmp::Reverse(integer(&v["id"]).unwrap_or(0)));
        let names: BTreeMap<i64, Value> = if rows.is_empty() {
            BTreeMap::new()
        } else {
            self.db
                .all("SELECT id,name FROM accounts WHERE deleted_at IS NULL", &[])
                .await?
                .into_iter()
                .filter_map(|v| integer(&v["id"]).map(|id| (id, v["name"].clone())))
                .collect()
        };
        rows.retain(|v| integer(&v["account_id"]).is_some_and(|id| names.contains_key(&id)));
        let items: Vec<_> = rows
            .iter()
            .take(limit)
            .map(|row| {
                let mut value = pick(
                    row,
                    &[
                        "id",
                        "account_id",
                        "test_completed_at",
                        "recovered_at",
                        "legacy",
                    ],
                );
                value["account_name"] = clean(
                    if truthy(&row["account_name"]) {
                        &row["account_name"]
                    } else {
                        names
                            .get(&integer(&row["account_id"]).unwrap_or(0))
                            .unwrap_or(&Value::Null)
                    },
                    160,
                )
                .into();
                value["model_id"] = clean(&row["model_id"], 160).into();
                if row["kind"] == "quota_recovery" {
                    value["kind"] = "quota_recovery".into();
                }
                if row["kind"] == "connection_test" {
                    value["kind"] = "connection_test".into();
                    value["result_status"] = clean(&row["result_status"], 64).into();
                }
                let receipt = &row["reset_credit"];
                if row["kind"] == "reset_credit"
                    && row["legacy"] == false
                    && receipt.is_object()
                    && receipt["consumed"] == true
                    && matches!(
                        string(&receipt["verification_method"]),
                        "connection" | "model"
                    )
                    && [
                        &receipt["completed_at"],
                        &row["test_completed_at"],
                        &row["recovered_at"],
                    ]
                    .into_iter()
                    .all(|v| parse_datetime(v).is_some())
                {
                    value["kind"] = "reset_credit".into();
                    value["reset_credit"] = pick(
                        receipt,
                        &["consumed", "completed_at", "verification_method"],
                    );
                }
                value
            })
            .collect();
        Ok(
            json!({"next_cursor":if rows.len() > limit {items.last().map(|v|v["id"].clone()).unwrap_or(Value::Null)} else {Value::Null},"items":items}),
        )
    }
}
