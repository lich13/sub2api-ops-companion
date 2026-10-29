//! Direct credential replacement. Credentials never enter the operation queue or state files.
use crate::{
    compat::string,
    error::{Error, Result},
    runtime::Runtime,
    validation,
};
use serde_json::{json, Value};

const CREDENTIALS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token_type",
    "expires_at",
    "scope",
    "email",
    "email_address",
    "chatgpt_account_id",
    "chatgpt_user_id",
    "organization_id",
    "plan_type",
    "subscription_expires_at",
    "client_id",
    "sub",
    "team_id",
    "org_uuid",
    "account_uuid",
    "subscription_tier",
    "entitlement_status",
];
const EXTRA: &[&str] = &[
    "email",
    "email_address",
    "org_uuid",
    "account_uuid",
    "subscription_tier",
    "entitlement_status",
    "name",
    "privacy_mode",
];

pub fn validate(body: &Value) -> Result<()> {
    validation::fields(body, &["credentials", "extra"], &["credentials"])?;
    for (key, fields) in [("credentials", CREDENTIALS), ("extra", EXTRA)] {
        let Some(value) = body.get(key) else { continue };
        validation::fields(value, fields, &[])?;
        for (name, value) in value.as_object().ok_or_else(Error::invalid)? {
            if value.is_string() {
                validation::text(value, 1, 32768)?;
            } else if !(matches!(name.as_str(), "expires_at" | "subscription_expires_at")
                && value.as_u64().is_some()
                || key == "extra" && name == "privacy_mode" && value.is_boolean())
            {
                return Err(Error::invalid());
            }
        }
    }
    validation::text(&body["credentials"]["access_token"], 1, 32768)?;
    Ok(())
}

fn eligible(row: &Value, id: i64) -> Result<()> {
    if row["id"] != id || row["platform"] != "openai" || row["type"] != "oauth" {
        return Err(Error::new(
            409,
            "仅 OpenAI OAuth 账号支持重新授权，请刷新账号核对",
        ));
    }
    Ok(())
}

fn applied(row: &Value, body: &Value) -> bool {
    ["credentials", "extra"].iter().all(|section| {
        body[*section]
            .as_object()
            .into_iter()
            .flatten()
            .all(|(key, value)| {
                let current = &row[*section][key];
                current == value
                    || value.is_number() && current.as_str() == Some(value.to_string().as_str())
            })
    })
}

impl Runtime {
    pub async fn apply_oauth_credentials(&self, id: i64, key: &str, body: &Value) -> Result<Value> {
        self.writable()?;
        validate(body)?;
        let _control = self.actions.locks.try_control(id)?;
        let local = self.actions.account(id, None).await?;
        eligible(&local, id)?;
        let _lease = self.actions.locks.try_account(&local)?;
        let path = format!("accounts/{id}");
        let current = self
            .actions
            .json(reqwest::Method::GET, &path, key, None, 30)
            .await?;
        eligible(&current, id)?;
        if !applied(&current, body) {
            // Native merging preserves mappings, groups and proxy settings, including
            // edits made in Sub2API while this authorization dialog was open.
            let mut native = body.clone();
            native["type"] = "oauth".into();
            self.actions
                .json(
                    reqwest::Method::POST,
                    &format!("{path}/apply-oauth-credentials"),
                    key,
                    Some(&native),
                    120,
                )
                .await
                .map_err(|error| {
                    let rejected = [400, 401, 403, 404, 409, 422].iter().any(|status| {
                        error
                            .detail
                            .as_str()
                            .is_some_and(|text| text == format!("Sub2API 请求失败 [http_{status}]"))
                    });
                    Error::new(
                        502,
                        if rejected {
                            "授权保存被拒绝，请重新生成链接后授权"
                        } else {
                            "授权保存结果未确认，请刷新账号核对；未重放请求"
                        },
                    )
                })?;
        }
        let confirmed = self
            .actions
            .json(reqwest::Method::GET, &path, key, None, 30)
            .await
            .map_err(|_| Error::new(502, "授权回读未完成，请刷新账号核对；未重放请求"))?;
        if eligible(&confirmed, id).is_err() || !applied(&confirmed, body) {
            return Err(Error::new(
                409,
                "授权结果已变化或未确认，请刷新账号核对；未重放请求",
            ));
        }
        self.invalidate_snapshot().await;
        Ok(json!({"account_id":id,"verified":true,"status":string(&confirmed["status"])}))
    }
}
