//! SQL targets the existing Sub2API schema; no companion tables are required.
use crate::db::OptionalColumns;
use std::{
    borrow::Cow,
    sync::{LazyLock, OnceLock},
};

const PLAIN_UPSTREAM_MSG: &str =
    r####"CASE WHEN e.upstream_error_message !~ '^\s*\{' THEN e.upstream_error_message END"####;
const PLAIN_ERROR_MSG: &str =
    r####"CASE WHEN e.error_message !~ '^\s*\{' THEN e.error_message END"####;
const STREAM_OR_PROVIDER_MSG: &str =
    r####"CASE WHEN e.stream IS TRUE OR e.error_owner='provider' THEN e.error_message END"####;
const BODY_ERROR_MSG: &str = r####"substring(e.error_body FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')"####;
const BODY_RESPONSE_ERROR_MSG: &str = r####"CASE WHEN e.error_body ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring(e.error_body FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END"####;
const DETAIL_ERROR_MSG: &str = r####"substring(e.upstream_error_detail FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')"####;
const DETAIL_RESPONSE_ERROR_MSG: &str = r####"CASE WHEN e.upstream_error_detail ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring(e.upstream_error_detail FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END"####;

const EVENT_TEXT_EXPRS: &[&str] = &[
    "event->>'message'",
    "event->>'error_message'",
    "event->'error'->>'message'",
    "event->'response'->'error'->>'message'",
];

fn rpm_limit_match(expr: &str) -> String {
    format!(
        r####"{expr} ~* '您\s*已\s*(?:达到|超过)\s*请求(?:数|数量)\s*限制[^\n]{{0,120}}(?:1\s*分钟内|每\s*(?:1\s*)?分钟)\s*(?:最多|至多)?\s*请求\s*[0-9]+\s*次' OR {expr} ~* 'you\s*(?:have\s*)?(?:reached|exceeded)\s*(?:the\s*)?(?:request\s*)?(?:rate\s*)?limit[^\n]{{0,120}}[0-9]+\s*(?:requests?|reqs?)\s*(?:per|/|a)\s*minute' OR {expr} ~* '(?:request\s*(?:rate\s*)?limit|maximum\s+(?:number\s+of\s+)?requests?|requests?\s*(?:rate\s*)?limit)[^\n]{{0,120}}(?:maximum\s*)?[0-9]+\s*(?:requests?|reqs?)\s*(?:per|/|a)\s*minute'"####
    )
}

fn concurrency_limit_match(expr: &str) -> String {
    format!(
        r####"{expr} ~* '(?:too\s+many\s+concurrent\s+requests\s+for\s+(?:user|client|api[ -]?key)|(?:user|client|api[ -]?key)\s+concurrency\s+limit\s+exceeded|(?:用户|客户端|密钥)\s*(?:并发请求|并发)\s*(?:数)?\s*(?:已)?(?:达到|超过|超出).{{0,20}}(?:限制|上限))'"####
    )
}

fn capacity_message_match(expr: &str) -> String {
    format!(
        "regexp_replace(regexp_replace(lower(btrim({expr})), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ('our servers are currently overloaded. please try again later','selected model is at capacity. please try a different model','stream disconnected before completion: concurrency limit exceeded for account, please retry later')"
    )
}

fn join_or(exprs: &[&str], pred: fn(&str) -> String) -> String {
    exprs
        .iter()
        .map(|e| pred(e))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn upstream_events_exists(ue: bool, predicate: &str) -> String {
    if ue {
        format!(
            "EXISTS (SELECT 1 FROM (SELECT to_jsonb(e.upstream_errors) AS val) ue, jsonb_array_elements(CASE WHEN jsonb_typeof(ue.val)='array' THEN ue.val ELSE '[]'::jsonb END) event WHERE {predicate})"
        )
    } else {
        "FALSE".to_owned()
    }
}

struct ErrorSqlVariant {
    local_throttle: String,
    desktop_where: String,
    degradation_where: String,
    other_where: String,
}

fn build_error_sql_variant(has_body: bool, has_detail: bool, ue: bool) -> ErrorSqlVariant {
    let mut local_exprs = vec![PLAIN_UPSTREAM_MSG, PLAIN_ERROR_MSG];
    let mut cap_exprs = vec![STREAM_OR_PROVIDER_MSG, "e.upstream_error_message"];
    if has_body {
        let extra = [BODY_ERROR_MSG, BODY_RESPONSE_ERROR_MSG];
        local_exprs.extend_from_slice(&extra);
        cap_exprs.extend_from_slice(&extra);
    }
    if has_detail {
        let extra = [DETAIL_ERROR_MSG, DETAIL_RESPONSE_ERROR_MSG];
        local_exprs.extend_from_slice(&extra);
        cap_exprs.extend_from_slice(&extra);
    }
    let event_cap = upstream_events_exists(ue, &join_or(EVENT_TEXT_EXPRS, capacity_message_match));
    let local_throttle = format!(
        "(COALESCE((({}) OR {} OR (e.error_owner IN ('client','platform') AND e.error_phase IN ('concurrency','rate_limit','admission') AND ({} OR {}))), FALSE) AND NOT COALESCE(({}) OR {event_cap}, FALSE))",
        join_or(&local_exprs, rpm_limit_match),
        upstream_events_exists(ue, &join_or(EVENT_TEXT_EXPRS, rpm_limit_match)),
        join_or(&local_exprs, concurrency_limit_match),
        upstream_events_exists(ue, &join_or(EVENT_TEXT_EXPRS, concurrency_limit_match)),
        join_or(&local_exprs, capacity_message_match),
    );
    let capacity_evidence = |owner_phase: &str| {
        format!(
            "(e.account_id IS NOT NULL AND {owner_phase} AND EXISTS (SELECT 1 FROM accounts evidence_account WHERE evidence_account.id=e.account_id AND evidence_account.platform='openai' AND evidence_account.type IN ('oauth','apikey')) AND ({} OR {event_cap}))",
            join_or(&cap_exprs, capacity_message_match)
        )
    };
    let desktop_where = format!(
        "((e.account_id IS NOT NULL AND e.error_phase IN ('upstream', 'account_auth') AND e.error_owner='provider') OR {}) AND NOT {local_throttle}",
        capacity_evidence("(e.error_owner='platform' AND e.error_phase='internal' AND e.error_source='gateway')")
    );
    let deg_candidate = capacity_evidence(
        "((e.error_owner='platform' AND e.error_phase='internal' AND e.error_source='gateway') OR (e.error_owner='provider' AND e.error_phase='upstream'))",
    );
    let degradation_where =
        format!("NOT COALESCE({local_throttle}, FALSE) AND COALESCE({deg_candidate}, FALSE)");
    let other_where =
        format!("NOT COALESCE({local_throttle}, FALSE) AND NOT COALESCE({deg_candidate}, FALSE)");
    ErrorSqlVariant {
        local_throttle,
        desktop_where,
        degradation_where,
        other_where,
    }
}

static ERROR_SQL_VARIANTS: [OnceLock<ErrorSqlVariant>; 8] = [const { OnceLock::new() }; 8];

fn error_cols_idx(cols: OptionalColumns) -> usize {
    usize::from(cols.error_body)
        | (usize::from(cols.error_upstream_detail) << 1)
        | (usize::from(cols.error_upstream_errors) << 2)
}

fn error_variant_by_idx(idx: usize) -> &'static ErrorSqlVariant {
    ERROR_SQL_VARIANTS[idx]
        .get_or_init(|| build_error_sql_variant((idx & 1) != 0, (idx & 2) != 0, (idx & 4) != 0))
}

fn error_variant(cols: OptionalColumns) -> &'static ErrorSqlVariant {
    error_variant_by_idx(error_cols_idx(cols))
}

pub static ERRORS_ERROR_WHERE: LazyLock<&'static str> =
    LazyLock::new(|| error_variant_by_idx(7).desktop_where.as_str());
pub static ERRORS_DEGRADATION_WHERE: LazyLock<&'static str> =
    LazyLock::new(|| error_variant_by_idx(7).degradation_where.as_str());
pub static ERRORS_OTHER_WHERE: LazyLock<&'static str> =
    LazyLock::new(|| error_variant_by_idx(7).other_where.as_str());
pub static ERRORS_LOCAL_THROTTLE: LazyLock<&'static str> =
    LazyLock::new(|| error_variant_by_idx(7).local_throttle.as_str());

pub(crate) fn local_throttle_where(cols: OptionalColumns) -> &'static str {
    error_variant(cols).local_throttle.as_str()
}

pub(crate) fn desktop_error_where(cols: OptionalColumns) -> &'static str {
    error_variant(cols).desktop_where.as_str()
}

pub(crate) fn category_error_where(
    category: Option<&str>,
    cols: OptionalColumns,
) -> Option<&'static str> {
    let v = error_variant(cols);
    match category {
        None => Some(v.desktop_where.as_str()),
        Some("degradation") => Some(v.degradation_where.as_str()),
        Some("other") => Some(v.other_where.as_str()),
        _ => None,
    }
}
pub const DESKTOP_ERROR_FIELDS: &str = r####"e.id, e.account_id, e.group_id, e.created_at, e.platform, e.model,
 e.requested_model, e.upstream_model, e.status_code, e.upstream_status_code,
 e.provider_error_code, e.error_type, e.error_message, e.upstream_error_message,
 e.error_owner,e.error_phase,e.error_source,
 to_jsonb(e.upstream_errors) AS upstream_errors,
 e.request_id, e.resolved, a.name AS account_name, g.name AS group_name"####;

const DESKTOP_ERROR_FIELDS_NO_UE: &str = r####"e.id, e.account_id, e.group_id, e.created_at, e.platform, e.model,
 e.requested_model, e.upstream_model, e.status_code, e.upstream_status_code,
 e.provider_error_code, e.error_type, e.error_message, e.upstream_error_message,
 e.error_owner,e.error_phase,e.error_source,
 NULL::jsonb AS upstream_errors,
 e.request_id, e.resolved, a.name AS account_name, g.name AS group_name"####;

pub(crate) fn desktop_error_fields(cols: OptionalColumns) -> &'static str {
    if cols.error_upstream_errors {
        DESKTOP_ERROR_FIELDS
    } else {
        DESKTOP_ERROR_FIELDS_NO_UE
    }
}

pub fn build_account_sql(
    cols: OptionalColumns,
    private_prefix: &str,
    with_parent_join: bool,
    filter: &str,
) -> Cow<'static, str> {
    let parent_expr = if cols.account_parent_id {
        "nullif(a.parent_account_id::text,'')::bigint"
    } else {
        "NULL::bigint"
    };
    let sw_end = if cols.account_session_window_end {
        "a.session_window_end"
    } else {
        "NULL::text"
    };
    let sw_status = if cols.account_session_window_status {
        "a.session_window_status::text"
    } else {
        "NULL::text"
    };
    let parent_join = if with_parent_join {
        if cols.account_parent_id {
            " LEFT JOIN accounts p ON p.id=coalesce(nullif(a.parent_account_id::text,'')::bigint,nullif(a.extra->>'parent_account_id','')::bigint) AND p.deleted_at IS NULL "
        } else {
            " LEFT JOIN accounts p ON p.id=nullif(a.extra->>'parent_account_id','')::bigint AND p.deleted_at IS NULL "
        }
    } else {
        ""
    };
    let err_where = desktop_error_where(cols);
    Cow::Owned(format!(
        "SELECT {private_prefix}a.id, a.name, a.platform, a.type, a.status, a.schedulable, a.updated_at, a.priority, a.concurrency, a.load_factor, a.error_message, a.extra, \
         {parent_expr} AS parent_account_id, \
         coalesce(nullif(a.credentials->>'plan_type',''),nullif(a.credentials->>'chatgpt_plan_type',''),a.extra->>'plan_type') AS quota_plan_type, \
         a.credentials->>'account_mode' AS usage_account_mode, a.credentials->>'base_url' AS usage_base_url, \
         {sw_end} AS usage_session_window_end, {sw_status} AS usage_session_window_status, \
         a.proxy_id, md5(a.credentials::text) AS credential_version, \
         md5((a.credentials - 'model_mapping')::text) AS credential_settings_version, \
         jsonb_build_object('plan_type',a.credentials->'plan_type','chatgpt_account_id',a.credentials->'chatgpt_account_id','email',a.credentials->'email') AS account_identity, \
         a.credentials->'model_mapping' AS model_mapping, \
         nullif(a.credentials->>'project_id','') IS NOT NULL AS model_sync_project, \
         md5(coalesce(a.credentials->'model_mapping','{{}}'::jsonb)::text) AS model_mapping_version, \
         (SELECT md5(jsonb_agg(jsonb_build_array(g.id,g.model_allowlist) ORDER BY g.id)::text) \
          FROM account_groups ag JOIN groups g ON g.id=ag.group_id AND g.deleted_at IS NULL WHERE ag.account_id=a.id) AS model_catalog_version, \
         a.credentials->>'subscription_tier' AS quota_grok_tier, a.credentials->>'entitlement_status' AS quota_grok_entitlement, \
         a.temp_unschedulable_until, a.temp_unschedulable_reason, a.rate_limited_at, a.rate_limit_reset_at, a.overload_until, a.expires_at, a.auto_pause_on_expired, \
         coalesce(a.extra->>'grok_needs_reauth','false') = 'true' AS needs_reauth, \
         ARRAY(SELECT ag.group_id FROM account_groups ag JOIN groups g ON g.id=ag.group_id AND g.deleted_at IS NULL WHERE ag.account_id=a.id ORDER BY ag.group_id) AS group_ids, \
         u.created_at AS last_success_at, u.created_at AS last_called_at, e.created_at AS last_error_at, e.id AS last_error_id, \
         e.provider_error_code AS last_error_code, coalesce(e.upstream_status_code,e.status_code) AS last_error_status, \
         coalesce(nullif(e.upstream_error_message,''),e.error_message) AS last_error_message \
         FROM accounts a{parent_join} \
         LEFT JOIN LATERAL (SELECT created_at FROM usage_logs WHERE account_id=a.id ORDER BY created_at DESC,id DESC LIMIT 1) u ON true \
         LEFT JOIN LATERAL (SELECT e.created_at,e.id,e.provider_error_code,e.upstream_status_code,e.status_code,e.upstream_error_message,e.error_message FROM ops_error_logs e WHERE e.account_id=a.id AND {err_where} ORDER BY e.created_at DESC,e.id DESC LIMIT 1) e ON true \
         WHERE a.deleted_at IS NULL {filter} ORDER BY a.id"
    ))
}

pub(crate) fn build_usage_only_sql(cols: OptionalColumns) -> Cow<'static, str> {
    let parent_expr = if cols.account_parent_id {
        "nullif(a.parent_account_id::text,'')::bigint"
    } else {
        "NULL::bigint"
    };
    let sw_end = if cols.account_session_window_end {
        "a.session_window_end"
    } else {
        "NULL::text"
    };
    let sw_status = if cols.account_session_window_status {
        "a.session_window_status::text"
    } else {
        "NULL::text"
    };
    let parent_join = if cols.account_parent_id {
        " LEFT JOIN accounts p ON p.id=coalesce(nullif(a.parent_account_id::text,'')::bigint,nullif(a.extra->>'parent_account_id','')::bigint) AND p.deleted_at IS NULL "
    } else {
        " LEFT JOIN accounts p ON p.id=nullif(a.extra->>'parent_account_id','')::bigint AND p.deleted_at IS NULL "
    };
    Cow::Owned(format!(
        "SELECT a.id,a.platform,a.type,a.extra,a.credentials,p.id AS effective_parent_id,p.credentials AS parent_credentials, \
         {parent_expr} AS parent_account_id, \
         coalesce(nullif(a.credentials->>'plan_type',''),nullif(a.credentials->>'chatgpt_plan_type',''),a.extra->>'plan_type') AS quota_plan_type, \
         a.credentials->>'subscription_tier' AS quota_grok_tier,a.credentials->>'entitlement_status' AS quota_grok_entitlement, \
         a.credentials->>'account_mode' AS usage_account_mode,a.credentials->>'base_url' AS usage_base_url, \
         {sw_end} AS usage_session_window_end,{sw_status} AS usage_session_window_status \
         FROM accounts a{parent_join}WHERE a.id=$1 AND a.deleted_at IS NULL"
    ))
}

static ACCOUNT_LIST_SQL_CACHE: [OnceLock<String>; 64] = [const { OnceLock::new() }; 64];

fn account_cache_idx(cols: OptionalColumns) -> usize {
    let a = usize::from(cols.account_parent_id)
        | (usize::from(cols.account_session_window_end) << 1)
        | (usize::from(cols.account_session_window_status) << 2);
    error_cols_idx(cols) * 8 + a
}

pub(crate) fn desktop_accounts_sql(cols: OptionalColumns) -> Cow<'static, str> {
    let idx = account_cache_idx(cols);
    let cached = ACCOUNT_LIST_SQL_CACHE[idx]
        .get_or_init(|| build_account_sql(cols, "", false, "").into_owned());
    Cow::Borrowed(cached.as_str())
}

static ERRORS_LIST_SQL_CACHE: [OnceLock<String>; 96] = [const { OnceLock::new() }; 96];

pub(crate) fn errors_list_sql(
    cols: OptionalColumns,
    category: Option<&str>,
    has_account: bool,
    has_before: bool,
) -> Option<Cow<'static, str>> {
    let cat_idx = match category {
        None => 0usize,
        Some("degradation") => 1,
        Some("other") => 2,
        _ => return None,
    };
    let shape = usize::from(has_account) | (usize::from(has_before) << 1);
    let slot = (error_cols_idx(cols) * 3 + cat_idx) * 4 + shape;
    let sql = ERRORS_LIST_SQL_CACHE[slot].get_or_init(|| {
        let filter = category_error_where(category, cols).expect("validated category");
        let mut clauses = String::new();
        let mut idx = 0usize;
        if has_account {
            idx += 1;
            clauses.push_str(&format!(" AND e.account_id=${idx}"));
        }
        if has_before {
            idx += 1;
            clauses.push_str(&format!(" AND e.id<${idx}"));
        }
        idx += 1;
        format!(
            "SELECT {} FROM ops_error_logs e LEFT JOIN accounts a ON a.id=e.account_id LEFT JOIN groups g ON g.id=e.group_id WHERE {filter}{clauses} ORDER BY e.id DESC LIMIT ${idx}",
            desktop_error_fields(cols)
        )
    });
    Some(Cow::Borrowed(sql.as_str()))
}

pub const DESKTOP_GROUP_SQL: &str = r####"
SELECT g.id,g.name,g.platform,g.sort_order,u.id AS log_id,u.account_id,
 a.name AS account_name,u.model,u.upstream_model,u.created_at AS called_at
FROM groups g LEFT JOIN LATERAL (
 SELECT latest.* FROM (
   SELECT DISTINCT ON (l.account_id) l.id,l.account_id,l.model,l.upstream_model,l.created_at
   FROM usage_logs l JOIN accounts live ON live.id=l.account_id AND live.deleted_at IS NULL
   WHERE l.group_id=g.id
     AND EXISTS (
       SELECT 1 FROM account_groups current_membership
       WHERE current_membership.group_id=g.id
         AND current_membership.account_id=l.account_id
     )
   ORDER BY l.account_id,l.created_at DESC,l.id DESC
 ) latest ORDER BY created_at DESC,id DESC LIMIT 3
) u ON true LEFT JOIN accounts a ON a.id=u.account_id
WHERE g.deleted_at IS NULL ORDER BY g.sort_order,g.id,u.created_at DESC,u.id DESC
"####;

pub const USAGE_SELECT: &str = r####"u.id,u.user_id,u.api_key_id,u.account_id,u.group_id,u.request_id,u.upstream_request_id,u.model,u.requested_model,u.upstream_model,u.upstream_response_model,u.model_mapping_chain,u.upstream_model_mismatch,u.reasoning_effort,u.requested_reasoning_effort,u.request_type,u.stream,u.openai_ws_mode,u.input_tokens,u.output_tokens,u.cache_creation_tokens,u.cache_read_tokens,u.cache_creation_5m_tokens,u.cache_creation_1h_tokens,u.input_cost,u.output_cost,u.cache_creation_cost,u.cache_read_cost,u.total_cost,u.actual_cost,u.account_stats_cost,u.account_rate_multiplier,u.rate_multiplier,u.first_token_ms,u.duration_ms,u.created_at,u.user_agent,u.ip_address,u.inbound_endpoint,u.upstream_endpoint,u.billing_type,u.billing_mode,u.service_tier,u.image_count,u.image_output_tokens,u.image_output_cost,u.image_input_tokens,u.image_input_cost,u.video_count,u.video_duration_seconds,u.video_resolution,
 p.username AS user_name,p.email AS user_email,k.name AS api_key_name,
 a.name AS account_name,g.name AS group_name"####;

pub const USAGE_JOINS: &str = r####"LEFT JOIN users p ON p.id=u.user_id
 LEFT JOIN api_keys k ON k.id=u.api_key_id
 LEFT JOIN accounts a ON a.id=u.account_id
 LEFT JOIN groups g ON g.id=u.group_id"####;

pub const USAGE_REQUEST_TYPE: &str = r####"CASE WHEN u.request_type IN (1,2,3) THEN u.request_type WHEN u.openai_ws_mode THEN 3 WHEN u.stream THEN 2 ELSE 1 END"####;

pub const USAGE_MISMATCH: &str = r####"(u.upstream_model_mismatch IS TRUE OR
 (nullif(u.upstream_response_model,'') IS NOT NULL AND u.upstream_response_model <>
 coalesce(nullif(u.upstream_model,''),nullif(u.requested_model,''),nullif(u.model,''))))"####;

pub const STATS_STATS_SQL: &str = r####"
SELECT w.account_id,w.key,s.* FROM jsonb_to_recordset(%(windows)s::jsonb)
 AS w(account_id bigint,key text,start_at timestamptz)
LEFT JOIN LATERAL (
 SELECT count(*) AS requests,
 coalesce(sum(input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens),0) AS tokens,
 coalesce(sum(coalesce(account_stats_cost,total_cost) * coalesce(account_rate_multiplier,1)),0) AS cost,
 coalesce(sum(total_cost),0) AS standard_cost, coalesce(sum(actual_cost),0) AS user_cost
 FROM usage_logs WHERE account_id=w.account_id AND created_at >= w.start_at
) s ON true
"####;

static CAPACITY_POLL_SQL_CACHE: [OnceLock<String>; 8] = [const { OnceLock::new() }; 8];
static CAPACITY_RECENT_SQL_CACHE: [OnceLock<String>; 8] = [const { OnceLock::new() }; 8];

fn build_capacity_poll_query(cols: OptionalColumns, where_clause: &str) -> String {
    const CANDIDATE_COND: &str = "a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey') AND ((e.error_owner='provider' AND e.error_phase='upstream') OR (e.error_owner='platform' AND e.error_phase='internal' AND e.error_source='gateway'))";
    let mut inner_cols = String::from(
        "id,account_id,created_at,error_owner,error_phase,error_source,stream,requested_model,model,upstream_model,upstream_error_message,error_message",
    );
    let body = if cols.error_body {
        inner_cols.push_str(",error_body");
        format!("CASE WHEN {CANDIDATE_COND} THEN e.error_body END")
    } else {
        "NULL::text".to_owned()
    };
    let detail = if cols.error_upstream_detail {
        inner_cols.push_str(",upstream_error_detail");
        format!("CASE WHEN {CANDIDATE_COND} THEN e.upstream_error_detail END")
    } else {
        "NULL::text".to_owned()
    };
    let ue = if cols.error_upstream_errors {
        inner_cols.push_str(",upstream_errors");
        format!("CASE WHEN {CANDIDATE_COND} THEN to_jsonb(e.upstream_errors) END")
    } else {
        "NULL::jsonb".to_owned()
    };
    format!(
        "SELECT e.id,e.account_id,e.created_at,e.error_owner,e.error_phase,e.error_source,e.stream,e.requested_model,e.model,e.upstream_model,\
         e.upstream_error_message,e.error_message,{body} AS error_body,{detail} AS upstream_error_detail,{ue} AS upstream_errors,\
         a.name AS account_name,a.platform AS account_platform,a.type AS account_type,a.deleted_at AS account_deleted_at \
         FROM (SELECT {inner_cols} FROM ops_error_logs WHERE {where_clause} ORDER BY id LIMIT 200) e \
         LEFT JOIN accounts a ON a.id=e.account_id ORDER BY e.id"
    )
}

pub(crate) fn capacity_poll_sql(cols: OptionalColumns) -> Cow<'static, str> {
    let idx = error_cols_idx(cols);
    let sql = CAPACITY_POLL_SQL_CACHE[idx].get_or_init(|| build_capacity_poll_query(cols, "id>$1"));
    Cow::Borrowed(sql.as_str())
}

pub(crate) fn capacity_recent_sql(cols: OptionalColumns) -> Cow<'static, str> {
    let idx = error_cols_idx(cols);
    let sql = CAPACITY_RECENT_SQL_CACHE[idx].get_or_init(|| {
        build_capacity_poll_query(
            cols,
            "created_at>=$1 AND id>$2 AND id<=$4 AND NOT (id = ANY($3))",
        )
    });
    Cow::Borrowed(sql.as_str())
}

pub(crate) fn evidence_error_fields(cols: OptionalColumns) -> String {
    let body = if cols.error_body {
        "to_jsonb(e.error_body)"
    } else {
        "NULL::jsonb"
    };
    let detail = if cols.error_upstream_detail {
        "to_jsonb(e.upstream_error_detail)"
    } else {
        "NULL::jsonb"
    };
    let ue = if cols.error_upstream_errors {
        "to_jsonb(e.upstream_errors)"
    } else {
        "NULL::jsonb"
    };
    format!(
        "e.id,e.created_at,e.error_owner,e.error_phase,e.model,e.requested_model,e.upstream_model,\
         e.provider_error_code,e.provider_error_type,e.error_type,e.upstream_error_message,e.error_message,\
         {body} AS error_body,{detail} AS upstream_error_detail,{ue} AS upstream_errors"
    )
}

macro_rules! build_capacity_slow_sql {
    ($vc_col:expr, $vd_col:expr, $vc_pred:expr, $vd_pred:expr) => {
        concat!(
            "SELECT r.id,a.id AS account_id,r.created_at,r.model,r.upstream_model,r.stream,",
            "r.first_token_ms,r.duration_ms,r.output_tokens,r.inbound_endpoint,",
            "r.image_count,r.image_output_tokens,r.video_count,r.video_duration_seconds,",
            "a.name AS account_name,a.platform AS account_platform,a.type AS account_type ",
            "FROM accounts a LEFT JOIN LATERAL (",
            "  SELECT u.id,u.created_at,u.model,u.upstream_model,u.stream,",
            "         u.first_token_ms,u.duration_ms,u.output_tokens,u.inbound_endpoint,",
            "         u.image_count,u.image_output_tokens,",
            $vc_col,
            " AS video_count,",
            $vd_col,
            " AS video_duration_seconds ",
            "  FROM usage_logs u ",
            "  WHERE u.account_id=a.id AND u.created_at >= $1 AND u.created_at <= $2 ",
            "    AND u.stream IS TRUE AND u.first_token_ms > 0 AND u.duration_ms > u.first_token_ms ",
            "    AND u.output_tokens >= 0 AND coalesce(u.image_count,0)=0 AND coalesce(u.image_output_tokens,0)=0 ",
            $vc_pred,
            $vd_pred,
            "    AND coalesce(u.upstream_model,u.model,'') <> '' ",
            "    AND (coalesce(u.inbound_endpoint,'') || ' ' || coalesce(u.upstream_model,u.model,'')) !~* '(/audio|/images|/videos|-image|-video|-tts|-stt|realtime|whisper|dall-e)' ",
            "  ORDER BY u.created_at DESC,u.id DESC LIMIT 10",
            ") r ON true ",
            "WHERE a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey') ",
            "ORDER BY account_id,r.created_at DESC,r.id DESC"
        )
    };
}

pub const CAPACITY_SLOW_SQL: &str = build_capacity_slow_sql!(
    "u.video_count",
    "u.video_duration_seconds",
    "AND coalesce(u.video_count,0)=0 ",
    "AND coalesce(u.video_duration_seconds,0)=0 "
);
const CAPACITY_SLOW_SQL_VC_ONLY: &str = build_capacity_slow_sql!(
    "u.video_count",
    "0::double precision",
    "AND coalesce(u.video_count,0)=0 ",
    ""
);
const CAPACITY_SLOW_SQL_VD_ONLY: &str = build_capacity_slow_sql!(
    "0::bigint",
    "u.video_duration_seconds",
    "",
    "AND coalesce(u.video_duration_seconds,0)=0 "
);
const CAPACITY_SLOW_SQL_NO_VIDEO: &str =
    build_capacity_slow_sql!("0::bigint", "0::double precision", "", "");

pub(crate) fn capacity_slow_sql(cols: OptionalColumns) -> Cow<'static, str> {
    Cow::Borrowed(match (cols.usage_video_count, cols.usage_video_duration) {
        (true, true) => CAPACITY_SLOW_SQL,
        (true, false) => CAPACITY_SLOW_SQL_VC_ONLY,
        (false, true) => CAPACITY_SLOW_SQL_VD_ONLY,
        (false, false) => CAPACITY_SLOW_SQL_NO_VIDEO,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_optional_column_subsets_reference_exact_existing_columns() {
        for bits in 0..=u8::MAX {
            let cols = OptionalColumns::from_bits(bits);
            assert_eq!(cols.to_bits(), bits);

            for sql in [
                desktop_error_where(cols),
                local_throttle_where(cols),
                category_error_where(Some("degradation"), cols).unwrap(),
                category_error_where(Some("other"), cols).unwrap(),
            ] {
                assert_eq!(sql.contains("e.error_body"), cols.error_body);
                assert_eq!(
                    sql.contains("e.upstream_error_detail"),
                    cols.error_upstream_detail
                );
                assert_eq!(
                    sql.contains("e.upstream_errors"),
                    cols.error_upstream_errors
                );
            }

            let accounts = desktop_accounts_sql(cols);
            assert_eq!(
                accounts.contains("a.parent_account_id"),
                cols.account_parent_id
            );
            assert_eq!(
                accounts.contains("a.session_window_end"),
                cols.account_session_window_end
            );
            assert_eq!(
                accounts.contains("a.session_window_status"),
                cols.account_session_window_status
            );
            assert_eq!(accounts.contains("e.error_body"), cols.error_body);
            assert_eq!(
                accounts.contains("e.upstream_error_detail"),
                cols.error_upstream_detail
            );
            assert_eq!(
                accounts.contains("e.upstream_errors"),
                cols.error_upstream_errors
            );

            for cat in [None, Some("degradation"), Some("other")] {
                for has_account in [false, true] {
                    for has_before in [false, true] {
                        let list = errors_list_sql(cols, cat, has_account, has_before).unwrap();
                        assert_eq!(list.contains("e.error_body"), cols.error_body);
                        assert_eq!(
                            list.contains("e.upstream_error_detail"),
                            cols.error_upstream_detail
                        );
                        assert_eq!(
                            list.contains("e.upstream_errors"),
                            cols.error_upstream_errors
                        );
                    }
                }
            }

            let slow = capacity_slow_sql(cols);
            assert_eq!(slow.contains("u.video_count"), cols.usage_video_count);
            assert_eq!(
                slow.contains("u.video_duration_seconds"),
                cols.usage_video_duration
            );
        }
    }
}
