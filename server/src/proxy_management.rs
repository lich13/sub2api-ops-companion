//! Explicit proxy administration; credentials never enter inventory snapshots.
use crate::{
    actions::Actions,
    compat::{pick, string},
    error::{Error, Result},
    security::clean,
    upstream::read_limited,
    validation,
};
use reqwest::Method;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};

const FIELDS: &[&str] = &[
    "name",
    "protocol",
    "host",
    "port",
    "username",
    "password",
    "status",
    "expires_at",
    "fallback_mode",
    "backup_proxy_id",
    "expiry_warn_days",
];
const DATA_FIELDS: &[&str] = &[
    "proxy_key",
    "name",
    "protocol",
    "host",
    "port",
    "username",
    "password",
    "status",
    "expires_at",
    "fallback_mode",
    "backup_proxy_name",
    "expiry_warn_days",
];
fn choices(value: &Value, options: &[&str]) -> Result<()> {
    if options.contains(&string(value)) {
        Ok(())
    } else {
        Err(Error::invalid())
    }
}
fn proxy_input(value: &Value, create: bool, data: bool) -> Result<()> {
    validation::fields(
        value,
        if data { DATA_FIELDS } else { FIELDS },
        if create {
            &["protocol", "host", "port"]
        } else {
            &[]
        },
    )?;
    for field in [
        "name",
        "host",
        "username",
        "password",
        "proxy_key",
        "backup_proxy_name",
    ] {
        if let Some(v) = value.get(field) {
            let min = usize::from(matches!(field, "name" | "host"));
            let text = validation::text(v, min, if field == "proxy_key" { 2048 } else { 512 })?;
            if text.chars().any(char::is_control) || min > 0 && text.trim().is_empty() {
                return Err(Error::invalid());
            }
        }
    }
    if let Some(v) = value.get("host") {
        let host = string(v);
        if host.contains(['/', '@', '?', '#']) || host.chars().any(char::is_whitespace) {
            return Err(Error::invalid());
        }
    }
    if let Some(v) = value.get("protocol") {
        choices(v, &["http", "https", "socks5", "socks5h"])?;
    }
    if let Some(v) = value.get("status") {
        choices(
            v,
            if data {
                &["active", "inactive", "expired"]
            } else {
                &["active", "inactive"]
            },
        )?;
    }
    if let Some(v) = value.get("fallback_mode") {
        choices(v, &["none", "proxy", "direct"])?;
    }
    if let Some(v) = value.get("port") {
        validation::integer(v, 1, 65535)?;
    }
    if let Some(v) = value.get("expiry_warn_days") {
        validation::integer(v, 0, i32::MAX as i64)?;
    }
    for field in ["expires_at", "backup_proxy_id"] {
        if let Some(v) = value.get(field).filter(|v| !v.is_null()) {
            validation::integer(v, 1, i64::MAX)?;
        }
    }
    Ok(())
}
fn ids(value: &Value) -> Result<()> {
    let a = value
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 1000)
        .ok_or_else(Error::invalid)?;
    for v in a {
        validation::integer(v, 1, i64::MAX)?;
    }
    Ok(())
}
pub fn validate_body(path: &[&str], method: &str, value: &Value) -> Result<()> {
    match path {
        ["proxy-management"] if method == "POST" => {
            proxy_input(value, true, false)?;
            validation::text(&value["name"], 1, 512)?;
        }
        ["proxy-management", "batch"] => {
            validation::fields(value, &["proxies"], &["proxies"])?;
            let a = value["proxies"]
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 1000)
                .ok_or_else(Error::invalid)?;
            for p in a {
                validation::fields(
                    p,
                    &["protocol", "host", "port", "username", "password"],
                    &["protocol", "host", "port"],
                )?;
                proxy_input(p, true, false)?;
            }
        }
        ["proxy-management", "batch-delete"] => {
            validation::fields(value, &["ids"], &["ids"])?;
            ids(&value["ids"])?;
        }
        ["proxy-management", "data"] if method == "POST" => {
            validation::fields(value, &["data"], &["data"])?;
            let d = &value["data"];
            validation::fields(
                d,
                &["type", "version", "exported_at", "proxies", "accounts"],
                &["proxies"],
            )?;
            if d.get("accounts")
                .is_some_and(|a| !a.as_array().is_some_and(|a| a.is_empty()))
            {
                return Err(Error::new(422, "仅支持代理数据，不导入账号"));
            }
            if let Some(v) = d.get("type") {
                validation::text(v, 0, 80)?;
            }
            if let Some(v) = d.get("version") {
                validation::integer(v, 1, 1)?;
            }
            if let Some(v) = d.get("exported_at") {
                validation::text(v, 0, 80)?;
            }
            let a = d["proxies"]
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 1000)
                .ok_or_else(Error::invalid)?;
            for p in a {
                proxy_input(p, true, true)?;
            }
        }
        ["proxy-management", id] if method == "PUT" => {
            parse_id(id)?;
            proxy_input(value, false, false)?;
        }
        _ => return Err(Error::invalid()),
    }
    Ok(())
}
fn parse_id(id: &str) -> Result<i64> {
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::invalid());
    }
    id.parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(Error::invalid)
}
pub fn query_string(query: &BTreeMap<String, String>, export: bool) -> Result<String> {
    let mut url = reqwest::Url::parse("http://127.0.0.1").expect("static URL");
    for (k, v) in query {
        let valid = match k.as_str() {
            "page" => !export && v.parse::<u32>().is_ok_and(|v| v > 0),
            "page_size" => !export && v.parse::<u32>().is_ok_and(|v| (1..=200).contains(&v)),
            "protocol" => ["http", "https", "socks5", "socks5h"].contains(&v.as_str()),
            "status" => ["active", "inactive", "expired"].contains(&v.as_str()),
            "sort_by" => ["id", "name", "protocol", "account_count"].contains(&v.as_str()),
            "sort_order" => ["asc", "desc"].contains(&v.as_str()),
            "search" => v.chars().count() <= 100 && !v.chars().any(char::is_control),
            "ids" => {
                export && v.split(',').count() <= 1000 && v.split(',').all(|s| parse_id(s).is_ok())
            }
            _ => false,
        };
        if !valid {
            return Err(Error::invalid());
        }
        url.query_pairs_mut().append_pair(k, v);
    }
    let encoded = url.query().unwrap_or("").to_owned();
    Ok(if encoded.is_empty() {
        String::new()
    } else {
        format!("?{encoded}")
    })
}
pub fn safe_proxy(value: &Value) -> Result<Value> {
    validation::integer(&value["id"], 1, i64::MAX)
        .map_err(|_| Error::new(502, "代理响应格式无效"))?;
    for key in ["name", "protocol", "host", "status"] {
        validation::text(&value[key], 0, 512).map_err(|_| Error::new(502, "代理响应格式无效"))?;
    }
    validation::integer(&value["port"], 1, 65535).map_err(|_| Error::new(502, "代理端口无效"))?;
    let mut out = pick(
        value,
        &[
            "id",
            "name",
            "protocol",
            "host",
            "port",
            "status",
            "created_at",
            "updated_at",
            "expires_at",
            "fallback_mode",
            "backup_proxy_id",
            "expiry_warn_days",
            "account_count",
            "latency_ms",
            "latency_status",
            "ip_address",
            "country",
            "country_code",
            "region",
            "city",
            "quality_status",
            "quality_score",
            "quality_grade",
            "quality_checked",
        ],
    );
    for key in [
        "name",
        "country",
        "country_code",
        "region",
        "city",
        "quality_grade",
    ] {
        out[key] = clean(&out[key], 160).into();
    }
    out["username_set"] = (!string(&value["username"]).is_empty()).into();
    out["password_set"] = (!string(&value["password"]).is_empty()).into();
    Ok(out)
}
async fn upstream(
    actions: &Actions,
    key: &str,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let response = actions
        .request(method, path, key, body, Duration::from_secs(90), false)
        .await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::new(
            if status == 404 {
                404
            } else if (400..500).contains(&status) {
                409
            } else {
                502
            },
            match status {
                404 => "代理不存在或上游不支持此操作",
                400 | 409 => "代理操作被拒绝，请检查关联账号、备用代理和表单内容",
                _ => "代理操作未确认，请刷新核对；未重放请求",
            },
        ));
    }
    let bytes = read_limited(response, 8_000_000).await?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::new(502, "代理响应无效，请刷新核对；未重放请求"))?;
    if body["code"] != 0 {
        return Err(Error::new(502, "上游未确认代理操作"));
    }
    Ok(body["data"].clone())
}
pub async fn handle(
    actions: &Actions,
    key: &str,
    path: &[&str],
    method: Method,
    query: &BTreeMap<String, String>,
    body: &Value,
) -> Result<Value> {
    if method != Method::GET && actions.settings.read_only {
        return Err(Error::read_only());
    }
    let suffix = match path {
        ["proxy-management"] => String::new(),
        ["proxy-management", "batch" | "batch-delete" | "data"] => format!("/{}", path[1]),
        ["proxy-management", id] => format!("/{}", parse_id(id)?),
        ["proxy-management", id, "credentials" | "accounts"] => format!(
            "/{}{}",
            parse_id(id)?,
            if path[2] == "accounts" {
                "/accounts"
            } else {
                ""
            }
        ),
        _ => return Err(Error::invalid()),
    };
    let qs = if method == Method::GET && (suffix.is_empty() || suffix == "/data") {
        query_string(query, suffix == "/data")?
    } else if !query.is_empty() {
        return Err(Error::invalid());
    } else {
        String::new()
    };
    let data = upstream(
        actions,
        key,
        method.clone(),
        &format!("proxies{suffix}{qs}"),
        (method == Method::POST || method == Method::PUT).then_some(body),
    )
    .await?;
    if method == Method::DELETE {
        return Ok(json!({"deleted":true}));
    }
    match path {
        ["proxy-management"] if method == Method::GET => {
            let rows = data["items"]
                .as_array()
                .filter(|a| a.len() <= 200)
                .ok_or_else(|| Error::new(502, "代理列表格式无效"))?;
            let mut out = pick(&data, &["total", "page", "page_size", "pages"]);
            out["items"] = rows
                .iter()
                .map(safe_proxy)
                .collect::<Result<Vec<_>>>()?
                .into();
            Ok(out)
        }
        ["proxy-management", _, "credentials"] => {
            Ok(json!({"username":string(&data["username"]),"password":string(&data["password"])}))
        }
        ["proxy-management", _, "accounts"] => {
            let rows = data
                .as_array()
                .ok_or_else(|| Error::new(502, "关联账号响应无效"))?;
            Ok(rows.iter().map(|r|json!({"id":r["id"],"name":clean(&r["name"],160),"platform":clean(&r["platform"],40),"type":clean(&r["type"],40)})).collect::<Vec<_>>().into())
        }
        ["proxy-management", "batch"] => Ok(pick(&data, &["created", "skipped"])),
        ["proxy-management", "batch-delete"] => Ok(
            json!({"deleted_ids":data["deleted_ids"],"skipped":data["skipped"].as_array().into_iter().flatten().map(|r|json!({"id":r["id"],"reason":if string(&r["reason"]).contains("account"){"代理仍有关联账号"}else{"代理未删除，请核对关联关系"}})).collect::<Vec<_>>()}),
        ),
        ["proxy-management", "data"] if method == Method::GET => {
            let rows = data["proxies"]
                .as_array()
                .ok_or_else(|| Error::new(502, "导出响应无效"))?;
            Ok(
                json!({"exported_at":data["exported_at"],"proxies":rows.iter().map(|r|Value::Object(DATA_FIELDS.iter().filter_map(|k|r.get(*k).filter(|v|!v.is_null()).map(|v|((*k).to_owned(),v.clone()))).collect())).collect::<Vec<_>>(),"accounts":[]}),
            )
        }
        ["proxy-management", "data"] => Ok(
            json!({"proxy_created":data["proxy_created"],"proxy_reused":data["proxy_reused"],"proxy_failed":data["proxy_failed"],"errors":data["errors"].as_array().into_iter().flatten().map(|r|json!({"name":clean(&r["name"],160),"message":"导入失败，请检查代理字段和备用代理"})).collect::<Vec<_>>()}),
        ),
        _ => safe_proxy(&data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_management_input_preserves_omission_and_rejects_unsupported_fields() {
        let keep = json!({"name":"fixture"});
        assert!(validate_body(&["proxy-management", "7"], "PUT", &keep).is_ok());
        assert!(keep.get("password").is_none());
        assert!(validate_body(
            &["proxy-management", "7"],
            "PUT",
            &json!({"username":"","password":"","expires_at":null,"backup_proxy_id":null})
        )
        .is_ok());
        assert!(validate_body(
            &["proxy-management"],
            "POST",
            &json!({"name":"fixture","protocol":"socks5h","host":"::1","port":1080})
        )
        .is_ok());
        for body in [
            json!({"password":null}),
            json!({"host":"user:fixture@127.0.0.1"}),
            json!({"status":"expired"}),
            json!({"port":65536}),
            json!({"arbitrary":true}),
        ] {
            assert!(validate_body(&["proxy-management", "7"], "PUT", &body).is_err());
        }
        assert!(validate_body(&["proxy-management","data"],"POST",&json!({"data":{"proxies":[{"protocol":"http","host":"127.0.0.1","port":8080}],"accounts":[{}]}})).is_err());
    }
    #[test]
    fn proxy_management_projection_excludes_secrets_and_query_rejects_escape_hatches() {
        let row = json!({"id":7,"name":"fixture","protocol":"socks5","host":"::1","port":1080,"status":"active","username":"fixture-user","password":"fixture-password","proxy_key":"fixture-password","account_count":3});
        let safe = safe_proxy(&row).unwrap();
        assert_eq!(safe["username_set"], true);
        assert_eq!(safe["password_set"], true);
        assert!(!safe.to_string().contains("fixture-password"));
        assert!(!safe.to_string().contains("fixture-user"));
        assert!(
            query_string(&BTreeMap::from([("search".into(), "a & b".into())]), false)
                .unwrap()
                .contains("search=a+%26+b")
        );
        assert!(query_string(
            &BTreeMap::from([("include_password".into(), "true".into())]),
            false
        )
        .is_err());
        assert!(query_string(&BTreeMap::from([("ids".into(), "7,+8".into())]), true).is_err());
    }
}
