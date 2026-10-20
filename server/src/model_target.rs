use crate::{
    actions::Actions,
    compat::{config_revision, integer, pick, string, truthy},
    error::{Error, Result},
    model_catalog::CLIENT_VERSION,
    model_test_stream::Target,
};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[0-9]+(?:\.[0-9]+){1,3}(?:-[0-9A-Za-z.]+)?\z").unwrap());
static TRAILER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([^();/]+);\s*[^()]*\)\s*$").unwrap());
pub fn valid_version(value: &Value) -> Option<String> {
    let text = if truthy(value) {
        crate::compat::text(value)
    } else {
        String::new()
    };
    let text = text.trim();
    (text.len() <= 64 && VERSION.is_match(text)).then(|| text.to_owned())
}
pub fn standard_agent(version: &str) -> String {
    format!("codex-tui/{version} (Ubuntu 22.4.0; x86_64) xterm-256color")
}
pub fn originator(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 64 || !value.bytes().all(|b| (32..=126).contains(&b)) {
        return None;
    }
    let lower = value.to_lowercase();
    if [
        "codex_cli_rs",
        "codex-tui",
        "codex_vscode",
        "codex_vscode_copilot",
        "codex_app",
        "codex_chatgpt_desktop",
        "codex_atlas",
        "codex_exec",
        "codex_sdk_ts",
    ]
    .contains(&lower.as_str())
    {
        Some(lower)
    } else if lower.starts_with("codex ") {
        Some(value.into())
    } else {
        None
    }
}
pub fn synced_agent(custom: &Value, version: &str) -> String {
    let text = if truthy(custom) {
        crate::compat::text(custom)
    } else {
        String::new()
    };
    if text.is_empty() || text.len() > 512 || !text.bytes().all(|b| (32..=126).contains(&b)) {
        return standard_agent(version);
    }
    let text = text.trim();
    let Some((client, rest)) = text.split_once('/') else {
        return standard_agent(version);
    };
    let trailer = TRAILER.captures(text);
    let origin = originator(client.trim())
        .or_else(|| trailer.as_ref().and_then(|m| originator(m[1].trim())));
    let Some(origin) = origin.filter(|_| !rest.trim().is_empty()) else {
        return standard_agent(version);
    };
    let tail = rest.find(' ').map(|index| &rest[index..]).unwrap_or("");
    let mut text = format!("{origin}/{version}{tail}");
    if let Some(trailer) = TRAILER.captures(&text) {
        if originator(trailer[1].trim()).is_some() {
            text = format!(
                "{}({}; {version})",
                &text[..trailer.get(0).unwrap().start()],
                trailer[1].trim()
            );
        }
    }
    text
}
pub fn identity(values: &Value) -> (String, String) {
    let candidate = valid_version(&values["openai_codex_client_version"])
        .or_else(|| valid_version(&values["openai_codex_client_version_synced"]))
        .unwrap_or_else(|| CLIENT_VERSION.into());
    let mut parts = candidate
        .split('-')
        .next()
        .unwrap()
        .split('.')
        .map(|v| v.parse::<u64>().unwrap_or(u64::MAX))
        .collect::<Vec<_>>();
    parts.resize(3, 0);
    let version = if parts[..3] >= [0, 144, 0][..] {
        candidate
    } else {
        CLIENT_VERSION.into()
    };
    let agent = synced_agent(&values["openai_codex_user_agent"], &version);
    (version, agent)
}
pub fn signature(row: &Value) -> String {
    let mut stable = pick(
        row,
        &[
            "id",
            "parent_account_id",
            "platform",
            "type",
            "credentials",
            "proxy_id",
        ],
    );
    stable["protocol"] = pick(
        &row["extra"],
        &["openai_responses_mode", "openai_responses_supported"],
    );
    config_revision(&stable)
}
pub async fn account(actions: &Actions, id: i64) -> Result<Value> {
    let row=actions.queries.db.one("SELECT id,name,platform,type,status,schedulable,credentials,extra,proxy_id,nullif(to_jsonb(accounts)->>'parent_account_id','')::bigint AS parent_account_id,ARRAY(SELECT ag.group_id FROM account_groups ag JOIN groups g ON g.id=ag.group_id AND g.deleted_at IS NULL WHERE ag.account_id=accounts.id ORDER BY ag.group_id) AS group_ids FROM accounts WHERE id=$1 AND deleted_at IS NULL",&[id.into()]).await?;
    row.filter(|row| {
        row["platform"] == "openai" && matches!(string(&row["type"]), "oauth" | "apikey")
    })
    .ok_or_else(|| Error::new(422, "仅支持 Codex OAuth 和 Key 账号"))
}
pub async fn target(actions: &Actions, row: &Value, model: &str) -> Result<(Target, Value)> {
    let target = connection(actions, row, model).await?;
    if !actions.model_allowed(row, model).await? {
        return Err(Error::new(422, "所选模型不在当前账号上游模型目录中"));
    }
    if !crate::actions::text_model(model) {
        return Err(Error::new(422, "请选择该账号支持的文本模型"));
    }
    Ok(target)
}

/// Resolve the same credentials, protocol and proxy for discovery and inference.
/// Discovery does not validate its own candidate list recursively.
pub(crate) async fn connection(
    actions: &Actions,
    row: &Value,
    model: &str,
) -> Result<(Target, Value)> {
    let owner = if let Some(parent) = integer(&row["parent_account_id"])
        .or_else(|| integer(&row["extra"]["parent_account_id"]))
        .filter(|id| *id > 0)
    {
        account(actions, parent).await?
    } else {
        row.clone()
    };
    if truthy(&owner["parent_account_id"]) || truthy(&owner["extra"]["parent_account_id"]) {
        return Err(Error::new(422, "母账号关系无效"));
    }
    if owner["type"] != row["type"] {
        return Err(Error::new(422, "母账号凭据类型不一致"));
    }
    let oauth = owner["type"] == "oauth";
    let credentials = &owner["credentials"];
    let token = credentials[if oauth { "access_token" } else { "api_key" }]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::new(409, "账号凭据不可用"))?;
    let base = if oauth {
        "https://chatgpt.com/backend-api/codex"
    } else {
        credentials["base_url"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or("https://api.openai.com")
    };
    let explicit = base.ends_with('#');
    let base = base.strip_suffix('#').unwrap_or(base).trim_end_matches('/');
    let extra = &owner["extra"];
    let use_responses = oauth
        || extra["openai_responses_mode"] == "force_responses"
        || extra["openai_responses_mode"] != "force_chat_completions"
            && extra["openai_responses_supported"] != false;
    let endpoint = if use_responses {
        "/responses"
    } else {
        "/chat/completions"
    };
    let url = format!(
        "{base}{}{endpoint}",
        if oauth || explicit || base.ends_with("/v1") {
            ""
        } else {
            "/v1"
        }
    );
    let parsed = reqwest::Url::parse(&url).map_err(|_| Error::new(422, "上游地址无效"))?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(Error::new(422, "上游地址无效"));
    }
    let rows=actions.queries.db.all("SELECT key,value FROM settings WHERE key IN ('openai_codex_user_agent','openai_codex_client_version','openai_codex_client_version_synced')",&[]).await.unwrap_or_default();
    let values = rows
        .into_iter()
        .filter_map(|row| {
            row["key"]
                .as_str()
                .map(|key| (key.to_owned(), row["value"].clone()))
        })
        .collect::<serde_json::Map<_, _>>();
    let (version, agent) = identity(&Value::Object(values));
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in [
        ("authorization", format!("Bearer {token}")),
        ("content-type", "application/json".into()),
        ("accept", "text/event-stream".into()),
        ("user-agent", agent.clone()),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_static(key),
            value
                .parse()
                .map_err(|_| Error::new(409, "账号凭据不可用"))?,
        );
    }
    if oauth {
        for (key, value) in [
            ("openai-beta", "responses=experimental".into()),
            (
                "originator",
                originator(agent.split('/').next().unwrap_or("").trim())
                    .unwrap_or_else(|| "codex-tui".into()),
            ),
            ("version", version),
        ] {
            headers.insert(
                reqwest::header::HeaderName::from_static(key),
                value
                    .parse()
                    .map_err(|_| Error::new(409, "账号凭据不可用"))?,
            );
        }
        if truthy(&credentials["chatgpt_account_id"]) {
            headers.insert(
                "chatgpt-account-id",
                crate::compat::text(&credentials["chatgpt_account_id"])
                    .parse()
                    .map_err(|_| Error::new(409, "账号凭据不可用"))?,
            );
        }
    }
    let proxy_id = integer(&row["proxy_id"])
        .filter(|v| *v > 0)
        .or_else(|| integer(&owner["proxy_id"]).filter(|v| *v > 0));
    let proxy = if let Some(id) = proxy_id {
        let p=actions.queries.db.one("SELECT protocol,host,port,username,password FROM proxies WHERE id=$1 AND deleted_at IS NULL",&[id.into()]).await?.ok_or_else(||Error::new(409,"账号代理不可用"))?;
        let mut url = reqwest::Url::parse(&format!(
            "{}://{}:{}",
            string(&p["protocol"]),
            string(&p["host"]),
            integer(&p["port"]).unwrap_or(0)
        ))
        .map_err(|_| Error::new(409, "账号代理不可用"))?;
        if truthy(&p["username"]) {
            url.set_username(string(&p["username"]))
                .map_err(|_| Error::new(409, "账号代理不可用"))?;
            url.set_password(Some(string(&p["password"])))
                .map_err(|_| Error::new(409, "账号代理不可用"))?;
        }
        Some(url.to_string())
    } else {
        None
    };
    Ok((
        Target {
            url,
            headers,
            proxy,
            model: model.into(),
            oauth,
            protocol: if use_responses {
                "responses"
            } else {
                "chat_completions"
            }
            .into(),
        },
        owner,
    ))
}
