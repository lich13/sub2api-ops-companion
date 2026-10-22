//! Safe projections of Sub2API's native account usage responses.
use crate::{
    compat::{datetime, float, integer, parse_datetime, string, truthy},
    error::{Error, Result},
    quota::{nonnegative, window},
    runtime::Runtime,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

pub fn provider(row: &Value) -> &'static str {
    let platform = string(&row["platform"]);
    let kind = string(&row["type"]);
    let mode = string(&row["usage_account_mode"]);
    if kind == "apikey"
        && matches!(
            platform,
            "openai" | "anthropic" | "kimi" | "zhipu" | "deepseek" | "minimax"
        )
    {
        if let Ok(url) = reqwest::Url::parse(string(&row["usage_base_url"])) {
            if url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.port_or_known_default() == Some(443)
            {
                if matches!(url.host_str(), Some("ollama.com" | "www.ollama.com"))
                    && matches!(url.path(), "" | "/" | "/v1")
                {
                    return "ollama";
                }
                if url.host_str() == Some("opencode.ai")
                    && matches!(
                        url.path()
                            .trim_end_matches('/')
                            .to_ascii_lowercase()
                            .as_str(),
                        "/zen/go" | "/zen/go/v1"
                    )
                {
                    return "opencode";
                }
            }
        }
    }
    match (platform, kind) {
        ("anthropic", "oauth" | "setup-token") => "anthropic",
        ("gemini", "oauth" | "apikey" | "service_account") => "gemini",
        ("antigravity", "oauth") => "antigravity",
        ("kimi" | "zhipu" | "minimax", "apikey") if mode == "coding" => "cn_quota",
        ("opencode_go", "apikey") if mode != "zen" => "opencode",
        ("kimi" | "deepseek", "apikey") if mode != "coding" => "cn_balance",
        _ => "",
    }
}

fn timestamp(value: &Value) -> Value {
    parse_datetime(value)
        .or_else(|| {
            integer(value)
                .filter(|v| *v > 0)
                .and_then(|v| DateTime::from_timestamp(v, 0))
        })
        .map_or(Value::Null, |v| json!(datetime(v)))
}
#[allow(clippy::too_many_arguments)]
fn add(
    usage: &mut Value,
    key: &str,
    label: &str,
    percent: Option<f64>,
    reset: &Value,
    observed: &Value,
    at: DateTime<Utc>,
    source: &str,
) {
    usage["windows"].as_array_mut().unwrap().push(window(
        key,
        label,
        percent,
        &timestamp(reset),
        &timestamp(observed),
        at,
        source,
        false,
    ));
}
fn failure(usage: &mut Value, authorization: bool) {
    usage["status"] = if authorization {
        "authorization_required"
    } else {
        "error"
    }
    .into();
    usage["message"] = if authorization {
        "上游授权缺失或已失效，请在原站更新授权"
    } else {
        "上游用量读取失败，请重新查询"
    }
    .into();
    for item in usage["windows"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter(|w| w["source"] != "configured_quota")
    {
        item["status"] = "error".into();
    }
    for balance in usage["balances"].as_array_mut().into_iter().flatten() {
        balance["status"] = "error".into();
    }
}
fn finish(usage: &mut Value) {
    if matches!(string(&usage["status"]), "error" | "authorization_required") {
        return;
    }
    if usage["windows"].as_array().is_none_or(Vec::is_empty)
        && usage["balances"].as_array().is_none_or(Vec::is_empty)
    {
        usage["status"] = "unavailable".into();
        usage["message"] = "上游尚未提供用量窗口".into();
    } else {
        usage["status"] = "known".into();
    }
}
fn balances(entries: &[Value], observed: &Value, at: DateTime<Utc>) -> Vec<Value> {
    entries.iter().filter_map(|entry| {
        let balance = float(&entry["balance"]).filter(|v| v.is_finite())?;
        let status = window("", "", Some(0.0), &Value::Null, observed, at, "", false)["status"].clone();
        Some(json!({"currency":crate::security::clean(&entry["currency"],30),"balance":balance,"observed_at":timestamp(observed),"status":status}))
    }).collect()
}
fn balance_data(usage: &mut Value, data: &Value, observed: &Value, at: DateTime<Utc>) {
    let entries = data["balances"]
        .as_array()
        .cloned()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![data.clone()]);
    usage["balances"] = balances(&entries, observed, at).into();
}
fn apply_snapshot(usage: &mut Value, snapshot: &Value, kind: &str, at: DateTime<Utc>) {
    let observed = timestamp(&snapshot["fetched_at"]);
    let windows: &[(&str, &str)] = if kind == "ollama" {
        &[("five_hour", "5h"), ("seven_day", "7d")]
    } else {
        &[("rolling", "5h"), ("weekly", "7d"), ("monthly", "30d")]
    };
    for (key, label) in windows {
        let raw = &snapshot["data"][key];
        if !raw.is_object() {
            continue;
        }
        // Sub2API serializes absent OpenCode windows as a zero-valued Go struct.
        if kind == "opencode"
            && string(&raw["status"]).is_empty()
            && string(&raw["resets_at"]).starts_with("0001-01-01")
        {
            continue;
        }
        add(
            usage,
            &format!("{kind}_{key}"),
            label,
            nonnegative(
                &raw[if kind == "ollama" {
                    "used_percent"
                } else {
                    "percent"
                }],
            ),
            &raw[if kind == "ollama" {
                "reset_at"
            } else {
                "resets_at"
            }],
            &observed,
            at,
            "upstream_snapshot",
        );
    }
    if snapshot["status"].as_str().is_some_and(|s| s != "ok") {
        failure(usage, snapshot["status"] == "unauthorized");
    }
}

/// Local/passive data stays in /snapshot. No outbound reads are made here.
pub fn extend(row: &Value, usage: &mut Value, at: DateTime<Utc>) {
    let kind = provider(row);
    if kind.is_empty() {
        return;
    }
    let extra = &row["extra"];
    usage["native_read"] = true.into();
    if usage["branch"] == "none" {
        usage["branch"] = "native".into();
    }
    usage["actions"] = json!(["query_usage"]);
    match kind {
        "anthropic" => {
            let observed = timestamp(&extra["passive_usage_sampled_at"]);
            let reset = timestamp(&row["usage_session_window_end"]);
            if !reset.is_null() {
                let percent = nonnegative(&extra["session_window_utilization"]).map(|v| v * 100.0);
                let estimate = match string(&row["usage_session_window_status"]) {
                    "rejected" => Some(100.0),
                    "allowed_warning" => Some(80.0),
                    "allowed" => Some(0.0),
                    _ => None,
                };
                add(
                    usage,
                    "five_hour",
                    "5h",
                    percent.or(estimate),
                    &reset,
                    &observed,
                    at,
                    "passive",
                );
                let item = usage["windows"].as_array_mut().unwrap().last_mut().unwrap();
                item["estimated"] = percent.is_none().into();
            }
            for (key, label, prefix) in [
                ("seven_day", "7d", "passive_usage_7d"),
                ("seven_day_fable", "7d F", "passive_usage_7d_oi"),
            ] {
                if let Some(percent) = nonnegative(&extra[format!("{prefix}_utilization")]) {
                    add(
                        usage,
                        key,
                        label,
                        Some(percent * 100.0),
                        &extra[format!("{prefix}_reset")],
                        &observed,
                        at,
                        "passive",
                    );
                }
            }
            for item in usage["windows"].as_array_mut().unwrap() {
                if let Some(reset) = parse_datetime(&item["reset_at"]).filter(|v| *v > at) {
                    item["stats_start"] = datetime(
                        reset
                            - if item["key"] == "five_hour" {
                                Duration::hours(5)
                            } else {
                                Duration::days(7)
                            },
                    )
                    .into();
                }
            }
        }
        "cn_quota" => {
            let platform = string(&row["platform"]);
            let observed = timestamp(&extra[format!("{platform}_usage_updated_at")]);
            for (key, label) in [("5h", "5h"), ("weekly", "7d"), ("monthly", "30d")] {
                if let Some(percent) = nonnegative(&extra[format!("{platform}_{key}_used_percent")])
                {
                    add(
                        usage,
                        &format!("{platform}_{key}"),
                        label,
                        Some(percent),
                        &extra[format!("{platform}_{key}_reset_at")],
                        &observed,
                        at,
                        "upstream_snapshot",
                    );
                }
            }
        }
        "cn_balance" => {
            let platform = string(&row["platform"]);
            balance_data(
                usage,
                &json!({"balances":extra[format!("{platform}_balances")],"balance":extra[format!("{platform}_balance")],"currency":extra[format!("{platform}_balance_currency")]}),
                &timestamp(&extra[format!("{platform}_balance_updated_at")]),
                at,
            );
        }
        "ollama" | "opencode" => apply_snapshot(
            usage,
            &extra[if kind == "ollama" {
                "ollama_cloud_usage_snapshot"
            } else {
                "opencode_go_usage_snapshot"
            }],
            kind,
            at,
        ),
        _ => {}
    }
    finish(usage);
}

pub fn apply_native(row: &Value, usage: &mut Value, data: &Value, at: DateTime<Utc>) -> Result<()> {
    if !data.is_object() {
        return Err(Error::new(502, "用量响应格式无效"));
    }
    let kind = provider(row);
    if data["success"] == false
        || truthy(&data["error"])
        || truthy(&data["error_code"])
        || data["needs_reauth"] == true
        || data["is_forbidden"] == true
    {
        failure(
            usage,
            data["needs_reauth"] == true
                || data["is_forbidden"] == true
                || matches!(integer(&data["status_code"]), Some(401 | 403))
                || matches!(
                    string(&data["error_code"]),
                    "unauthenticated" | "unauthorized" | "forbidden"
                ),
        );
        return Ok(());
    }
    if matches!(kind, "ollama" | "opencode") {
        if kind == "ollama" && data["configured"] != true
            || data["snapshot"]["status"] == "unauthorized"
        {
            failure(usage, true);
            return Ok(());
        }
        if data["snapshot"]["status"] == "failed" {
            failure(usage, false);
            return Ok(());
        }
    }
    // Configured limits and per-account statistics are independent of native quotas.
    usage["windows"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["source"] == "configured_quota");
    usage.as_object_mut().unwrap().remove("message");
    usage.as_object_mut().unwrap().remove("balances");
    usage["status"] = "known".into();
    let observed = timestamp(&data["updated_at"]);
    match kind {
        "anthropic" | "gemini" | "antigravity" => {
            for (key, label) in [
                ("five_hour", "5h"),
                ("seven_day", "7d"),
                ("seven_day_sonnet", "7d S"),
                ("seven_day_fable", "7d F"),
                ("gemini_shared_daily", "共享 1d"),
                ("gemini_pro_daily", "Pro 1d"),
                ("gemini_flash_daily", "Flash 1d"),
                ("gemini_shared_minute", "共享 1m"),
                ("gemini_pro_minute", "Pro 1m"),
                ("gemini_flash_minute", "Flash 1m"),
            ] {
                let raw = &data[key];
                if !raw.is_object() {
                    continue;
                }
                // Anthropic's passive endpoint synthesizes 0% when it has no sample.
                if kind == "anthropic"
                    && data["source"] == "passive"
                    && observed.is_null()
                    && timestamp(&raw["resets_at"]).is_null()
                {
                    continue;
                }
                add(
                    usage,
                    key,
                    label,
                    nonnegative(&raw["utilization"]),
                    &raw["resets_at"],
                    &observed,
                    at,
                    if kind == "gemini" {
                        "local_estimate"
                    } else if data["source"] == "passive" {
                        "passive"
                    } else {
                        "upstream_usage"
                    },
                );
                let item = usage["windows"].as_array_mut().unwrap().last_mut().unwrap();
                if kind == "gemini" {
                    item["estimated"] = true.into();
                }
                let stats = &raw["window_stats"];
                if ["requests", "tokens", "cost", "standard_cost", "user_cost"]
                    .iter()
                    .all(|k| nonnegative(&stats[*k]).is_some())
                {
                    item["stats"] = json!({"requests":integer(&stats["requests"]),"tokens":integer(&stats["tokens"]),"cost":float(&stats["cost"]),"standard_cost":float(&stats["standard_cost"]),"user_cost":float(&stats["user_cost"])});
                }
            }
            if let Some(models) = data["antigravity_quota"].as_object() {
                for (model, raw) in models {
                    let label = crate::security::clean(&json!(model), 100);
                    add(
                        usage,
                        &format!("antigravity_{label}"),
                        &label,
                        nonnegative(&raw["utilization"]),
                        &raw["reset_time"],
                        &observed,
                        at,
                        "upstream_usage",
                    );
                }
            }
            if let Some(credits) = data["ai_credits"].as_array() {
                usage["balances"] = balances(
                    &credits
                        .iter()
                        .map(|v| json!({"currency":v["credit_type"],"balance":v["amount"]}))
                        .collect::<Vec<_>>(),
                    &observed,
                    at,
                )
                .into();
            }
        }
        "cn_quota" => {
            for tier in data["tiers"].as_array().into_iter().flatten() {
                let key = string(&tier["window"]);
                let label = match key {
                    "5h" => "5h",
                    "weekly" => "7d",
                    "monthly" => "30d",
                    _ => continue,
                };
                add(
                    usage,
                    &format!("{}_{key}", string(&row["platform"])),
                    label,
                    nonnegative(&tier["used_percent"]),
                    &tier["reset_at"],
                    &data["fetched_at"],
                    at,
                    "upstream_usage",
                );
            }
        }
        "cn_balance" => balance_data(usage, data, &timestamp(&data["fetched_at"]), at),
        "ollama" | "opencode" => apply_snapshot(usage, &data["snapshot"], kind, at),
        _ => {}
    }
    finish(usage);
    Ok(())
}

impl Runtime {
    pub async fn account_usage(&self, id: i64, key: &str, force: bool) -> Result<Value> {
        let row = self.actions.account(id, None).await?;
        let kind = provider(&row);
        let mut usage = self.account_snapshot(id).await?["usage"].clone();
        if kind.is_empty() {
            return Ok(usage);
        }
        let (method, path) = match kind {
            "anthropic" => (
                reqwest::Method::GET,
                format!(
                    "accounts/{id}/usage?source={}",
                    if force && row["type"] == "oauth" {
                        "active&force=true"
                    } else {
                        "passive"
                    }
                ),
            ),
            "gemini" | "antigravity" => (
                reqwest::Method::GET,
                format!(
                    "accounts/{id}/usage{}",
                    if force { "?force=true" } else { "" }
                ),
            ),
            "cn_quota" => (
                reqwest::Method::GET,
                format!("cn-providers/accounts/{id}/quota"),
            ),
            "cn_balance" => (
                reqwest::Method::GET,
                format!("cn-providers/accounts/{id}/balance"),
            ),
            "ollama" | "opencode" => (
                if force {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                format!(
                    "accounts/{id}/{}-usage{}",
                    if kind == "ollama" {
                        "ollama-cloud"
                    } else {
                        "opencode-go"
                    },
                    if force { "/refresh" } else { "" }
                ),
            ),
            _ => return Ok(usage),
        };
        let response = self.actions.json(method, &path, key, None, 30).await;
        let live = self.actions.account(id, None).await?;
        if row["credential_version"] != live["credential_version"]
            || row["platform"] != live["platform"]
            || row["type"] != live["type"]
            || row["parent_account_id"] != live["parent_account_id"]
        {
            return Err(Error::new(409, "账号已变化，请重新读取用量"));
        }
        match response {
            Ok(data) => apply_native(&row, &mut usage, &data, Utc::now())?,
            Err(error) => failure(
                &mut usage,
                matches!(error.status.as_u16(), 401 | 403)
                    || string(&error.detail).contains("[http_401]")
                    || string(&error.detail).contains("[http_403]"),
            ),
        }
        if force {
            self.invalidate().await;
        }
        Ok(usage)
    }
}
