//! SQL targets the existing Sub2API schema; no companion tables are required.

macro_rules! sql_plain_upstream_msg {
    () => {
        r####"CASE WHEN e.upstream_error_message !~ '^\s*\{' THEN e.upstream_error_message END"####
    };
}

macro_rules! sql_plain_error_msg {
    () => {
        r####"CASE WHEN e.error_message !~ '^\s*\{' THEN e.error_message END"####
    };
}

macro_rules! sql_stream_or_provider_msg {
    () => {
        r####"CASE WHEN e.stream IS TRUE OR e.error_owner='provider' THEN e.error_message END"####
    };
}

macro_rules! sql_body_error_msg {
    () => {
        r####"substring(e.error_body FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')"####
    };
}

macro_rules! sql_body_response_error_msg {
    () => {
        r####"CASE WHEN e.error_body ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring(e.error_body FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END"####
    };
}

macro_rules! sql_detail_error_msg {
    () => {
        r####"substring(e.upstream_error_detail FROM '^\s*\{\s*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"')"####
    };
}

macro_rules! sql_detail_response_error_msg {
    () => {
        r####"CASE WHEN e.upstream_error_detail ~ '^\s*\{[^{}]*"type"\s*:\s*"response\.(failed|incomplete)"' THEN substring(e.upstream_error_detail FROM '^\s*\{[^{}]*"response"\s*:\s*\{[^{}]*"error"\s*:\s*\{[^{}]*"message"\s*:\s*"([^"]*)"') END"####
    };
}

macro_rules! sql_upstream_events_exists {
    ($predicate:expr) => {
        concat!(
            "EXISTS (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(to_jsonb(e)->'upstream_errors')='array' THEN to_jsonb(e)->'upstream_errors' ELSE '[]'::jsonb END) event WHERE ",
            $predicate,
            ")"
        )
    };
}

macro_rules! sql_rpm_limit_match {
    ($expr:expr) => {
        concat!(
            $expr,
            r####" ~* '您\s*已\s*(?:达到|超过)\s*请求(?:数|数量)\s*限制[^\n]{0,120}(?:1\s*分钟内|每\s*(?:1\s*)?分钟)\s*(?:最多|至多)?\s*请求\s*[0-9]+\s*次' OR "####,
            $expr,
            r####" ~* 'you\s*(?:have\s*)?(?:reached|exceeded)\s*(?:the\s*)?(?:request\s*)?(?:rate\s*)?limit[^\n]{0,120}[0-9]+\s*(?:requests?|reqs?)\s*(?:per|/|a)\s*minute' OR "####,
            $expr,
            r####" ~* '(?:request\s*(?:rate\s*)?limit|maximum\s+(?:number\s+of\s+)?requests?|requests?\s*(?:rate\s*)?limit)[^\n]{0,120}(?:maximum\s*)?[0-9]+\s*(?:requests?|reqs?)\s*(?:per|/|a)\s*minute'"####
        )
    };
}

macro_rules! sql_concurrency_limit_match {
    ($expr:expr) => {
        concat!(
            $expr,
            r####" ~* '(?:too\s+many\s+concurrent\s+requests\s+for\s+(?:user|client|api[ -]?key)|(?:user|client|api[ -]?key)\s+concurrency\s+limit\s+exceeded|(?:用户|客户端|密钥)\s*(?:并发请求|并发)\s*(?:数)?\s*(?:已)?(?:达到|超过|超出).{0,20}(?:限制|上限))'"####
        )
    };
}

macro_rules! sql_capacity_message_match {
    ($expr:expr) => {
        concat!(
            "regexp_replace(regexp_replace(lower(btrim(",
            $expr,
            ")), '[[:space:]]+', ' ', 'g'), '[.!?。！？,，;；:：[:space:]]+$', '', 'g') IN ('our servers are currently overloaded. please try again later','selected model is at capacity. please try a different model','stream disconnected before completion: concurrency limit exceeded for account, please retry later')"
        )
    };
}

macro_rules! sql_join_or {
    ($m:ident; $first:expr $(, $rest:expr)+) => {
        concat!($m!($first) $(, " OR ", $m!($rest))+)
    };
}

macro_rules! sql_event_capacity_matches {
    () => {
        sql_upstream_events_exists!(sql_join_or!(
            sql_capacity_message_match;
            "event->>'message'",
            "event->>'error_message'",
            "event->'error'->>'message'",
            "event->'response'->'error'->>'message'"
        ))
    };
}

macro_rules! sql_local_throttle {
    () => {
        concat!(
            "(COALESCE(((",
            sql_join_or!(
                sql_rpm_limit_match;
                sql_plain_upstream_msg!(),
                sql_plain_error_msg!(),
                sql_body_error_msg!(),
                sql_body_response_error_msg!(),
                sql_detail_error_msg!(),
                sql_detail_response_error_msg!()
            ),
            ") OR ",
            sql_upstream_events_exists!(sql_join_or!(
                sql_rpm_limit_match;
                "event->>'message'",
                "event->>'error_message'",
                "event->'error'->>'message'",
                "event->'response'->'error'->>'message'"
            )),
            " OR (e.error_owner IN ('client','platform') AND e.error_phase IN ('concurrency','rate_limit','admission') AND (",
            sql_join_or!(
                sql_concurrency_limit_match;
                sql_plain_upstream_msg!(),
                sql_plain_error_msg!(),
                sql_body_error_msg!(),
                sql_body_response_error_msg!(),
                sql_detail_error_msg!(),
                sql_detail_response_error_msg!()
            ),
            " OR ",
            sql_upstream_events_exists!(sql_join_or!(
                sql_concurrency_limit_match;
                "event->>'message'",
                "event->>'error_message'",
                "event->'error'->>'message'",
                "event->'response'->'error'->>'message'"
            )),
            "))), FALSE) AND NOT COALESCE((",
            sql_join_or!(
                sql_capacity_message_match;
                sql_plain_upstream_msg!(),
                sql_plain_error_msg!(),
                sql_body_error_msg!(),
                sql_body_response_error_msg!(),
                sql_detail_error_msg!(),
                sql_detail_response_error_msg!()
            ),
            ") OR ",
            sql_event_capacity_matches!(),
            ", FALSE))"
        )
    };
}

macro_rules! sql_openai_capacity_evidence {
    ($owner_phase:expr) => {
        concat!(
            "(e.account_id IS NOT NULL AND ",
            $owner_phase,
            " AND EXISTS (SELECT 1 FROM accounts evidence_account WHERE evidence_account.id=e.account_id AND evidence_account.platform='openai' AND evidence_account.type IN ('oauth','apikey')) AND (",
            sql_join_or!(
                sql_capacity_message_match;
                sql_stream_or_provider_msg!(),
                "e.upstream_error_message",
                sql_body_error_msg!(),
                sql_body_response_error_msg!(),
                sql_detail_error_msg!(),
                sql_detail_response_error_msg!()
            ),
            " OR ",
            sql_event_capacity_matches!(),
            "))"
        )
    };
}

macro_rules! sql_desktop_error_where {
    () => {
        concat!(
            "((e.account_id IS NOT NULL AND e.error_phase IN ('upstream', 'account_auth') AND e.error_owner='provider') OR ",
            sql_openai_capacity_evidence!(
                "(e.error_owner='platform' AND e.error_phase='internal' AND e.error_source='gateway')"
            ),
            ") AND NOT ",
            sql_local_throttle!()
        )
    };
}

macro_rules! sql_degradation_candidate {
    () => {
        sql_openai_capacity_evidence!(
            "((e.error_owner='platform' AND e.error_phase='internal' AND e.error_source='gateway') OR (e.error_owner='provider' AND e.error_phase='upstream'))"
        )
    };
}

pub const DESKTOP_SUPPORTED: &str =
    r####"deleted_at IS NULL AND platform IN ('openai','grok') AND type IN ('oauth','apikey')"####;

pub const DESKTOP_ERROR_WHERE: &str = sql_desktop_error_where!();

pub const DESKTOP_ERROR_FIELDS: &str = r####"e.id, e.account_id, e.group_id, e.created_at, e.platform, e.model,
 e.requested_model, e.upstream_model, e.status_code, e.upstream_status_code,
 e.provider_error_code, e.error_type, e.error_message, e.upstream_error_message,
 e.error_owner,e.error_phase,e.error_source,
 to_jsonb(e)->'upstream_errors' AS upstream_errors,
 e.request_id, e.resolved, a.name AS account_name, g.name AS group_name"####;

pub const DESKTOP_ACCOUNT_SQL: &str = concat!(
    r####"
SELECT a.id, a.name, a.platform, a.type, a.status, a.schedulable, a.updated_at, a.priority, a.concurrency, a.load_factor, a.error_message, a.extra,
 nullif(to_jsonb(a)->>'parent_account_id','')::bigint AS parent_account_id,
 coalesce(nullif(a.credentials->>'plan_type',''),nullif(a.credentials->>'chatgpt_plan_type',''),a.extra->>'plan_type') AS quota_plan_type,
 a.credentials->>'account_mode' AS usage_account_mode, a.credentials->>'base_url' AS usage_base_url,
 to_jsonb(a)->>'session_window_end' AS usage_session_window_end, to_jsonb(a)->>'session_window_status' AS usage_session_window_status,
 a.proxy_id, md5(a.credentials::text) AS credential_version,
 md5((a.credentials - 'model_mapping')::text) AS credential_settings_version,
 a.credentials->'model_mapping' AS model_mapping,
 nullif(a.credentials->>'project_id','') IS NOT NULL AS model_sync_project,
 md5(coalesce(a.credentials->'model_mapping','{}'::jsonb)::text) AS model_mapping_version,
 (SELECT md5(jsonb_agg(jsonb_build_array(g.id,g.model_allowlist) ORDER BY g.id)::text)
  FROM account_groups ag JOIN groups g ON g.id=ag.group_id AND g.deleted_at IS NULL WHERE ag.account_id=a.id) AS model_catalog_version,
 a.credentials->>'subscription_tier' AS quota_grok_tier, a.credentials->>'entitlement_status' AS quota_grok_entitlement,
 a.temp_unschedulable_until, a.rate_limit_reset_at, a.overload_until, a.expires_at, a.auto_pause_on_expired,
 coalesce(a.extra->>'grok_needs_reauth','false') = 'true' AS needs_reauth,
 ARRAY(SELECT ag.group_id FROM account_groups ag JOIN groups g ON g.id=ag.group_id AND g.deleted_at IS NULL WHERE ag.account_id=a.id ORDER BY ag.group_id) AS group_ids,
 u.created_at AS last_success_at, u.created_at AS last_called_at, e.created_at AS last_error_at, e.id AS last_error_id,
 e.provider_error_code AS last_error_code, coalesce(e.upstream_status_code,e.status_code) AS last_error_status,
 coalesce(nullif(e.upstream_error_message,''),e.error_message) AS last_error_message
FROM accounts a
LEFT JOIN LATERAL (SELECT created_at FROM usage_logs WHERE account_id=a.id ORDER BY created_at DESC,id DESC LIMIT 1) u ON true
LEFT JOIN LATERAL (SELECT e.* FROM ops_error_logs e WHERE e.account_id=a.id AND "####,
    sql_desktop_error_where!(),
    r####" ORDER BY e.created_at DESC,e.id DESC LIMIT 1) e ON true
WHERE a.deleted_at IS NULL {filter} ORDER BY a.id
"####
);

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

pub const ERRORS_ERROR_WHERE: &str = DESKTOP_ERROR_WHERE;

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

pub const LEGACY_LEGACY_RECOVERY_PLAN_CLEANUP_SQL: &str = r####"
DELETE FROM scheduled_test_plans
WHERE cron_expression = '* * * * *'
  AND auto_recover = true
RETURNING id, account_id
"####;

pub const ERRORS_DEGRADATION_WHERE: &str = concat!(
    "NOT COALESCE(",
    sql_local_throttle!(),
    ", FALSE) AND COALESCE(",
    sql_degradation_candidate!(),
    ", FALSE)"
);

pub const ERRORS_OTHER_WHERE: &str = concat!(
    "NOT COALESCE(",
    sql_local_throttle!(),
    ", FALSE) AND NOT COALESCE(",
    sql_degradation_candidate!(),
    ", FALSE)"
);

pub const ERRORS_LOCAL_THROTTLE: &str = sql_local_throttle!();

pub const CAPACITY_FIELDS: &str = r####"e.id,e.account_id,e.created_at,e.error_owner,e.error_phase,e.error_source,e.stream,e.requested_model,e.model,e.upstream_model,
 e.upstream_error_message,e.error_message,to_jsonb(e)->'error_body' AS error_body,
 to_jsonb(e)->'upstream_error_detail' AS upstream_error_detail,
 to_jsonb(e)->'upstream_errors' AS upstream_errors,
 a.name AS account_name,a.platform AS account_platform,a.type AS account_type,a.deleted_at AS account_deleted_at"####;

// Keep the ordered candidate scan separate so expensive JSON/media filters
// are evaluated only until ten qualifying samples have been found. OFFSET 0
// prevents predicate pushdown across that boundary; ties still sort by id.
pub const CAPACITY_SLOW_SQL: &str = r####"
            SELECT r.id,a.id AS account_id,r.created_at,r.model,r.upstream_model,r.stream,
                   r.first_token_ms,r.duration_ms,r.output_tokens,r.inbound_endpoint,
                   r.image_count,r.image_output_tokens,r.video_count,r.video_duration_seconds,
                   a.name AS account_name,a.platform AS account_platform,a.type AS account_type
            FROM accounts a LEFT JOIN LATERAL (
              SELECT u.id,u.created_at,u.model,u.upstream_model,u.stream,
                     u.first_token_ms,u.duration_ms,u.output_tokens,u.inbound_endpoint,
                     u.image_count,u.image_output_tokens,
                     to_jsonb(u)->>'video_count' AS video_count,
                     to_jsonb(u)->>'video_duration_seconds' AS video_duration_seconds
              FROM (
                SELECT u.* FROM usage_logs u
                WHERE u.account_id=a.id AND u.created_at >= $1 AND u.created_at <= $2
                ORDER BY u.created_at DESC,u.id DESC OFFSET 0
              ) u
              WHERE u.stream IS TRUE AND u.first_token_ms > 0 AND u.duration_ms > u.first_token_ms
                AND u.output_tokens >= 0 AND coalesce(u.image_count,0)=0 AND coalesce(u.image_output_tokens,0)=0
                AND coalesce(to_jsonb(u)->>'video_count','0') IN ('0','0.0')
                AND coalesce(to_jsonb(u)->>'video_duration_seconds','0') IN ('0','0.0')
                AND coalesce(u.upstream_model,u.model,'') <> ''
                AND (coalesce(u.inbound_endpoint,'') || ' ' || coalesce(u.upstream_model,u.model,'')) !~* '(/audio|/images|/videos|-image|-video|-tts|-stt|realtime|whisper|dall-e)'
              ORDER BY u.created_at DESC,u.id DESC LIMIT 10
            ) r ON true
            WHERE a.deleted_at IS NULL AND a.platform='openai' AND a.type IN ('oauth','apikey')
            ORDER BY account_id,r.created_at DESC,r.id DESC
        "####;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicated_sql_constants_are_byte_for_byte_identical() {
        let expected = [
            (
                "DESKTOP_SUPPORTED",
                DESKTOP_SUPPORTED,
                83,
                "17813ec7dcc6a164715366f107c2b33c59e578526ec9387682912dee6e5d75a1",
            ),
            (
                "DESKTOP_ERROR_WHERE",
                DESKTOP_ERROR_WHERE,
                21382,
                "a32ea587d40dd1ffe26ac53ed395e555141057533a184dc53bf8a883d02a198a",
            ),
            (
                "DESKTOP_ERROR_FIELDS",
                DESKTOP_ERROR_FIELDS,
                393,
                "3a11331e2fd05e78c76b334fcbb50ce3f1de42bd8feb2bb14c156b5a4126a921",
            ),
            (
                "DESKTOP_ACCOUNT_SQL",
                DESKTOP_ACCOUNT_SQL,
                23682,
                "90e85737253b083d80575ed1cb9d9458886086422d42f0857a1d11538dac6844",
            ),
            (
                "DESKTOP_GROUP_SQL",
                DESKTOP_GROUP_SQL,
                838,
                "1deb56b6c6a93984d2c96b9b289698038e829acbb7a6ed8c0c9b6277c7a3275f",
            ),
            (
                "ERRORS_ERROR_WHERE",
                ERRORS_ERROR_WHERE,
                21382,
                "a32ea587d40dd1ffe26ac53ed395e555141057533a184dc53bf8a883d02a198a",
            ),
            (
                "USAGE_SELECT",
                USAGE_SELECT,
                1005,
                "5e1019a0fda0dfb77a0ab8fe82e99846bc5732f0f2d7e94c3dd2aa34f3ccdac0",
            ),
            (
                "USAGE_JOINS",
                USAGE_JOINS,
                160,
                "ab4826e98e675eb1b56f601a8586232a5255295836e93bed4140665e61348f83",
            ),
            (
                "USAGE_REQUEST_TYPE",
                USAGE_REQUEST_TYPE,
                116,
                "57b24a84a46ec78f247b2bec120e422596b101f288d157b636ad14f17b663efd",
            ),
            (
                "USAGE_MISMATCH",
                USAGE_MISMATCH,
                210,
                "2b5e14d3bc3150eff84af9a6463986b26e089561a4681182a2b1af4f2c628bf8",
            ),
            (
                "STATS_STATS_SQL",
                STATS_STATS_SQL,
                564,
                "21bc8f3277bcb67443d46f7dfab54640fabb701140b7e2353ba50c111021fc55",
            ),
            (
                "LEGACY_LEGACY_RECOVERY_PLAN_CLEANUP_SQL",
                LEGACY_LEGACY_RECOVERY_PLAN_CLEANUP_SQL,
                121,
                "843ba51a7732b7fa5891e9fe677f9aeb518aa82cbb1fc93369ca0bcf26e281bb",
            ),
            (
                "ERRORS_DEGRADATION_WHERE",
                ERRORS_DEGRADATION_WHERE,
                21366,
                "70dc5995129622813dbe192782a68e14325aa2b2923d3d3e30593d034815cf2e",
            ),
            (
                "ERRORS_OTHER_WHERE",
                ERRORS_OTHER_WHERE,
                21370,
                "8713a4bdca1ee17f90052a8f09e04791471fc38ea51197d78c59ff15dd6cae1a",
            ),
            (
                "ERRORS_LOCAL_THROTTLE",
                ERRORS_LOCAL_THROTTLE,
                16398,
                "05e4d3893ea35d4753f9ce3f42109a5b19707aea68b58231f5303d0e532a4629",
            ),
            (
                "CAPACITY_FIELDS",
                CAPACITY_FIELDS,
                438,
                "22097ef112ab041885025b2877af8e4a85dbef15f465c7e941ae030c8140a242",
            ),
            (
                "CAPACITY_SLOW_SQL",
                CAPACITY_SLOW_SQL,
                1890,
                "fcdf12a538018e44e338dc21cac3306bc8e5ebfecf3d427efb826832aed2c23a",
            ),
        ];
        for (name, sql, len, digest) in expected {
            assert_eq!(sql.len(), len, "{name} byte length changed");
            assert_eq!(
                crate::compat::sha256(sql),
                digest,
                "{name} sha256 digest changed"
            );
        }
    }
}
