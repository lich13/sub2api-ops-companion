use crate::compat::{integer, string};
use serde_json::{json, Value};
pub fn diagnostics(status: Option<u16>, data: &Value) -> Value {
    let mut out = json!({"http_status":status});
    let code = &data["code"];
    if code.is_i64() {
        out["business_code"] = code.clone();
    } else if let Some(code) = code.as_str() {
        let normalized = code.trim().to_lowercase();
        out["business_code"] = if [
            "reset",
            "nothing_to_reset",
            "no_credit",
            "already_redeemed",
            "success",
            "ok",
            "error",
            "failed",
            "redeemed",
        ]
        .contains(&normalized.as_str())
        {
            normalized
        } else if code.strip_prefix("OPENAI_").is_some_and(|v| {
            !v.is_empty() && v.len() <= 56 && v.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        }) {
            code.to_owned()
        } else {
            "unrecognized".into()
        }
        .into();
    }
    if data["windows_reset"].is_i64() {
        out["windows_reset"] = data["windows_reset"].clone();
    }
    for key in [
        "cache_persisted",
        "cache_refreshed",
        "account_state_recovered",
    ] {
        if data[key].is_boolean() {
            out[key] = data[key].clone();
        }
    }
    out
}
pub fn consumption_result(data: &Value) -> Value {
    let raw = &data["code"];
    let code = string(raw).trim().to_lowercase();
    let windows = integer(&data["windows_reset"]);
    let credit = &data["credit"];
    let credit_ok = credit.is_null()
        || credit.is_object()
            && (credit["status"].is_null() || matches!(string(&credit["status"]), "" | "redeemed"));
    let consumed = (raw.is_i64() && raw == 0
        || raw.is_string() && matches!(code.as_str(), "reset" | "success" | "ok"))
        && data["windows_reset"].is_i64()
        && windows.is_some_and(|n| n > 0)
        && credit_ok;
    let not_consumed = raw.is_string()
        && matches!(code.as_str(), "no_credit" | "nothing_to_reset")
        && data["windows_reset"].is_i64()
        && windows == Some(0)
        && (credit.is_null()
            || credit.is_object()
                && (credit["status"].is_null()
                    || matches!(string(&credit["status"]), "" | "available" | "expired")));
    json!({"success":consumed,"consumed":consumed,"uncertain":!(consumed || not_consumed),"error_code":if consumed {""} else if not_consumed || code=="already_redeemed" {&code} else {"result_uncertain"},"consumption_outcome":if consumed {"consumed"} else if not_consumed {"not_consumed"} else {"unknown"}})
}
