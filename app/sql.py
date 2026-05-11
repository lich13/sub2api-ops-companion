QUALITY_SQL = """
WITH group_accounts AS (
  SELECT
    a.id,
    a.name,
    a.platform,
    a.type,
    a.status,
    a.schedulable,
    a.priority AS account_priority,
    ag.priority AS group_priority,
    a.concurrency,
    a.last_used_at,
    a.updated_at,
    a.temp_unschedulable_until,
    a.temp_unschedulable_reason,
    a.rate_limited_at,
    a.rate_limit_reset_at,
    a.overload_until,
    a.session_window_start,
    a.session_window_end,
    a.session_window_status,
    a.error_message,
    CASE WHEN coalesce(a.extra->>'codex_5h_used_percent','') ~ '^-?[0-9]+([.][0-9]+)?$' THEN (a.extra->>'codex_5h_used_percent')::numeric END AS codex_5h_used_percent,
    a.extra->>'codex_5h_reset_at' AS codex_5h_reset_at,
    CASE WHEN coalesce(a.extra->>'codex_7d_used_percent','') ~ '^-?[0-9]+([.][0-9]+)?$' THEN (a.extra->>'codex_7d_used_percent')::numeric END AS codex_7d_used_percent,
    a.extra->>'codex_7d_reset_at' AS codex_7d_reset_at,
    CASE WHEN coalesce(a.extra->>'codex_primary_used_percent','') ~ '^-?[0-9]+([.][0-9]+)?$' THEN (a.extra->>'codex_primary_used_percent')::numeric END AS codex_primary_used_percent,
    a.extra->>'codex_primary_reset_after_seconds' AS codex_primary_reset_after_seconds,
    CASE WHEN coalesce(a.extra->>'codex_secondary_used_percent','') ~ '^-?[0-9]+([.][0-9]+)?$' THEN (a.extra->>'codex_secondary_used_percent')::numeric END AS codex_secondary_used_percent,
    a.extra->>'codex_secondary_reset_after_seconds' AS codex_secondary_reset_after_seconds,
    a.extra->>'codex_usage_updated_at' AS codex_usage_updated_at
  FROM accounts a
  JOIN account_groups ag ON ag.account_id = a.id
  JOIN groups g ON g.id = ag.group_id
  WHERE g.name = %(group_name)s
    AND a.deleted_at IS NULL
),
successes AS (
  SELECT
    account_id,
    count(*) FILTER (WHERE created_at >= now() - make_interval(hours => %(hours)s)) AS success_window,
    count(*) AS success_7d,
    max(created_at) AS last_success_at,
    round(avg(duration_ms)::numeric, 0) AS avg_duration_ms,
    round(avg(first_token_ms)::numeric, 0) AS avg_first_token_ms
  FROM usage_logs
  WHERE created_at >= now() - interval '7 days'
  GROUP BY account_id
),
lifetime_usage AS (
  SELECT
    account_id,
    count(*) AS lifetime_request_count,
    round(COALESCE(sum(total_cost),0), 6) AS lifetime_total_cost,
    round(COALESCE(sum(actual_cost),0), 6) AS lifetime_actual_cost,
    COALESCE(sum(
      COALESCE(input_tokens,0)
      + COALESCE(output_tokens,0)
      + COALESCE(cache_creation_tokens,0)
      + COALESCE(cache_read_tokens,0)
      + COALESCE(cache_creation_5m_tokens,0)
      + COALESCE(cache_creation_1h_tokens,0)
      + COALESCE(image_output_tokens,0)
    ),0) AS lifetime_total_tokens
  FROM usage_logs
  WHERE account_id IS NOT NULL
  GROUP BY account_id
),
raw_error_attempts AS (
  SELECT
    e.created_at,
    e.request_id,
    COALESCE(
      CASE WHEN coalesce(x.elem->>'account_id','') ~ '^[0-9]+$' THEN (x.elem->>'account_id')::bigint END,
      e.account_id
    ) AS account_id,
    COALESCE(
      CASE
        WHEN lower(trim(coalesce(x.elem->>'account_name',''))) IN ('', 'none', 'null') THEN NULL
        ELSE x.elem->>'account_name'
      END,
      a.name
    ) AS account_name,
    COALESCE(
      CASE WHEN coalesce(x.elem->>'upstream_status_code','') ~ '^[0-9]+$' THEN (x.elem->>'upstream_status_code')::int END,
      e.upstream_status_code,
      e.status_code
    ) AS status_code,
    COALESCE(NULLIF(x.elem->>'kind',''), e.error_type) AS kind,
    e.error_owner,
    e.error_source,
    COALESCE(
      NULLIF(x.elem->>'detail',''),
      NULLIF(x.elem->>'message',''),
      NULLIF(x.elem->>'upstream_response_body',''),
      e.upstream_error_message,
      e.error_message,
      e.error_body,
      ''
    ) AS message,
    concat_ws(
      ' ',
      NULLIF(x.elem->>'detail',''),
      NULLIF(x.elem->>'message',''),
      NULLIF(x.elem->>'upstream_response_body',''),
      e.upstream_error_message,
      e.error_message,
      e.error_body,
      x.elem::text,
      e.upstream_errors::text
    ) AS search_text
  FROM ops_error_logs e
  LEFT JOIN accounts a ON a.id = e.account_id
  LEFT JOIN LATERAL jsonb_array_elements(
    CASE
      WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
      ELSE '[{}]'::jsonb
    END
  ) AS x(elem) ON true
  WHERE e.created_at >= now() - interval '7 days'
    AND (%(platform)s = '' OR e.platform = %(platform)s)
),
classified AS (
  SELECT *,
    CASE
      WHEN account_id IS NULL THEN 'client_pre_route'
      WHEN error_owner = 'client' OR error_source = 'client_request' THEN 'client_request'
      WHEN status_code = 400 AND (
        search_text ILIKE '%%Input must be a list%%'
        OR search_text ILIKE '%%Instructions are required%%'
      ) THEN 'client_bad_request'
      WHEN search_text ILIKE '%%用户额度不足%%'
        OR search_text ILIKE '%%额度不足%%'
        OR search_text ILIKE '%%额度已用尽%%'
        OR search_text ILIKE '%%令牌额度已用尽%%'
        OR search_text ILIKE '%%预扣费额度失败%%'
        OR search_text ILIKE '%%剩余额度%%'
        OR search_text ~* 'RemainQuota[[:space:]]*=[[:space:]]*-'
        OR search_text ILIKE '%%insufficient_user_quota%%'
        OR search_text ILIKE '%%insufficient%%balance%%'
        OR search_text ILIKE '%%INSUFFICIENT_BALANCE%%'
        OR search_text ILIKE '%%not enough credits%%'
        OR search_text ILIKE '%%quota exceeded%%' THEN 'provider_balance_or_quota'
      WHEN status_code = 403 AND search_text ILIKE '%%blocked%%' THEN 'provider_blocked_403'
      WHEN status_code = 429
        OR search_text ILIKE '%%rate limit%%'
        OR search_text ILIKE '%%Too many pending%%'
        OR search_text ILIKE '%%quota%%' THEN 'provider_rate_limit'
      WHEN status_code BETWEEN 500 AND 599
        OR kind ILIKE '%%truncated%%'
        OR search_text ILIKE '%%terminal event%%'
        OR search_text ILIKE '%%missing terminal event%%' THEN 'upstream_unstable_5xx_stream'
      ELSE 'account_other_error'
    END AS category
  FROM raw_error_attempts
),
by_account AS (
  SELECT
    account_id,
    count(*) FILTER (
      WHERE created_at >= now() - make_interval(hours => %(hours)s)
        AND category NOT IN ('client_pre_route','client_request','client_bad_request')
    ) AS account_quality_errors_window,
    count(*) FILTER (WHERE category NOT IN ('client_pre_route','client_request','client_bad_request')) AS account_quality_errors_7d,
    count(*) FILTER (WHERE created_at >= now() - make_interval(hours => %(hours)s) AND category = 'provider_blocked_403') AS blocked_403_window,
    count(*) FILTER (WHERE created_at >= now() - make_interval(hours => %(hours)s) AND category = 'provider_balance_or_quota') AS balance_or_quota_window,
    count(*) FILTER (WHERE created_at >= now() - make_interval(hours => %(hours)s) AND category = 'provider_rate_limit') AS rate_limit_window,
    count(*) FILTER (WHERE created_at >= now() - make_interval(hours => %(hours)s) AND category = 'upstream_unstable_5xx_stream') AS unstable_5xx_stream_window,
    count(*) FILTER (WHERE created_at >= now() - make_interval(hours => %(hours)s) AND category = 'client_bad_request') AS client_bad_request_window,
    max(created_at) FILTER (WHERE category NOT IN ('client_pre_route','client_request','client_bad_request')) AS last_error_at
  FROM classified
  WHERE account_id IS NOT NULL
  GROUP BY account_id
),
last_error AS (
  SELECT DISTINCT ON (account_id)
    account_id,
    created_at,
    status_code,
    kind,
    category,
    replace(coalesce(message,''), E'\\n', ' ') AS message
  FROM classified
  WHERE account_id IS NOT NULL
    AND category NOT IN ('client_pre_route','client_request','client_bad_request')
  ORDER BY account_id, created_at DESC
)
SELECT
  ga.*,
  COALESCE(s.success_window,0) AS success_window,
  COALESCE(s.success_7d,0) AS success_7d,
  COALESCE(b.account_quality_errors_window,0) AS account_quality_errors_window,
  COALESCE(b.account_quality_errors_7d,0) AS account_quality_errors_7d,
  CASE WHEN COALESCE(s.success_window,0)+COALESCE(b.account_quality_errors_window,0) > 0
    THEN round(100 * COALESCE(b.account_quality_errors_window,0)::numeric / (COALESCE(s.success_window,0)+COALESCE(b.account_quality_errors_window,0)), 1)
    ELSE NULL END AS error_rate_window_pct,
  COALESCE(b.blocked_403_window,0) AS blocked_403_window,
  COALESCE(b.balance_or_quota_window,0) AS balance_or_quota_window,
  COALESCE(b.rate_limit_window,0) AS rate_limit_window,
  COALESCE(b.unstable_5xx_stream_window,0) AS unstable_5xx_stream_window,
  COALESCE(b.client_bad_request_window,0) AS client_bad_request_window,
  s.last_success_at,
  b.last_error_at,
  COALESCE(lu.lifetime_request_count,0) AS lifetime_request_count,
  COALESCE(lu.lifetime_total_cost,0) AS lifetime_total_cost,
  COALESCE(lu.lifetime_actual_cost,0) AS lifetime_actual_cost,
  COALESCE(lu.lifetime_total_tokens,0) AS lifetime_total_tokens,
  le.status_code AS last_error_status,
  le.kind AS last_error_kind,
  le.category AS last_error_category,
  le.message AS last_error_message,
  s.avg_duration_ms,
  s.avg_first_token_ms
FROM group_accounts ga
LEFT JOIN successes s ON s.account_id = ga.id
LEFT JOIN lifetime_usage lu ON lu.account_id = ga.id
LEFT JOIN by_account b ON b.account_id = ga.id
LEFT JOIN last_error le ON le.account_id = ga.id
ORDER BY ga.group_priority, ga.account_priority, ga.id;
"""


REQUESTS_SQL = """
WITH expanded AS (
  SELECT
    e.id,
    e.created_at,
    e.request_id,
    e.client_request_id,
    e.platform,
    e.model,
    e.requested_model,
    e.upstream_model,
    e.group_id,
    g.name AS group_name,
    e.user_id,
    u.email AS user_email,
    e.request_path,
    e.inbound_endpoint,
    e.upstream_endpoint,
    e.stream,
    e.status_code AS final_status_code,
    e.upstream_status_code AS final_upstream_status_code,
    e.severity,
    e.error_message,
    e.error_body,
    e.error_owner,
    e.error_source,
    e.error_type,
    e.error_phase,
    e.account_id AS final_account_id,
    fa.name AS final_account_name,
    x.ordinality AS attempt_no,
    COALESCE(
      CASE WHEN coalesce(x.elem->>'account_id','') ~ '^[0-9]+$' THEN (x.elem->>'account_id')::bigint END,
      e.account_id
    ) AS attempt_account_id,
    COALESCE(
      CASE
        WHEN lower(trim(coalesce(x.elem->>'account_name',''))) IN ('', 'none', 'null') THEN NULL
        ELSE x.elem->>'account_name'
      END,
      fa.name
    ) AS attempt_account_name,
    COALESCE(
      CASE WHEN coalesce(x.elem->>'upstream_status_code','') ~ '^[0-9]+$' THEN (x.elem->>'upstream_status_code')::int END,
      e.upstream_status_code,
      e.status_code
    ) AS attempt_status_code,
    COALESCE(NULLIF(x.elem->>'kind',''), e.error_type) AS attempt_kind,
    COALESCE(
      NULLIF(x.elem->>'detail',''),
      NULLIF(x.elem->>'message',''),
      NULLIF(x.elem->>'upstream_response_body',''),
      e.upstream_error_message,
      e.error_message,
      e.error_body,
      ''
    ) AS attempt_message,
    e.upstream_errors
  FROM ops_error_logs e
  LEFT JOIN users u ON u.id = e.user_id
  LEFT JOIN groups g ON g.id = e.group_id
  LEFT JOIN accounts fa ON fa.id = e.account_id
  LEFT JOIN LATERAL jsonb_array_elements(
    CASE
      WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
      ELSE '[{}]'::jsonb
    END
  ) WITH ORDINALITY AS x(elem, ordinality) ON true
  WHERE e.created_at >= now() - make_interval(hours => %(hours)s)
    AND (%(platform)s = '' OR e.platform = %(platform)s)
    AND (%(q)s = '' OR e.request_id = %(q)s OR e.client_request_id = %(q)s OR e.error_message ILIKE '%%' || %(q)s || '%%' OR e.error_body ILIKE '%%' || %(q)s || '%%')
    AND (
      %(account_id)s::bigint IS NULL
      OR e.account_id = %(account_id)s::bigint
      OR COALESCE(
        CASE WHEN coalesce(x.elem->>'account_id','') ~ '^[0-9]+$' THEN (x.elem->>'account_id')::bigint END,
        e.account_id
      ) = %(account_id)s::bigint
    )
)
SELECT *
FROM expanded
WHERE attempt_account_id IS NOT NULL
ORDER BY created_at DESC, id DESC, attempt_no ASC
LIMIT %(limit)s;
"""


GUARD_BALANCE_CANDIDATES_SQL = """
WITH raw_error_attempts AS (
  SELECT
    e.created_at,
    COALESCE(
      CASE WHEN coalesce(x.elem->>'account_id','') ~ '^[0-9]+$' THEN (x.elem->>'account_id')::bigint END,
      e.account_id
    ) AS account_id,
    COALESCE(
      NULLIF(x.elem->>'detail',''),
      NULLIF(x.elem->>'message',''),
      NULLIF(x.elem->>'upstream_response_body',''),
      e.upstream_error_message,
      e.error_message,
      e.error_body,
      ''
    ) AS message,
    concat_ws(
      ' ',
      NULLIF(x.elem->>'detail',''),
      NULLIF(x.elem->>'message',''),
      NULLIF(x.elem->>'upstream_response_body',''),
      e.upstream_error_message,
      e.error_message,
      e.error_body,
      x.elem::text,
      e.upstream_errors::text
    ) AS search_text
  FROM ops_error_logs e
  LEFT JOIN LATERAL jsonb_array_elements(
    CASE
      WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
      ELSE '[{}]'::jsonb
    END
  ) AS x(elem) ON true
  WHERE e.created_at >= now() - (%(lookback_minutes)s::text || ' minutes')::interval
),
balance_errors AS (
  SELECT *
  FROM raw_error_attempts
  WHERE account_id IS NOT NULL
    AND (
      search_text ILIKE '%%用户额度不足%%'
      OR search_text ILIKE '%%额度不足%%'
      OR search_text ILIKE '%%额度已用尽%%'
      OR search_text ILIKE '%%令牌额度已用尽%%'
      OR search_text ILIKE '%%预扣费额度失败%%'
      OR search_text ILIKE '%%剩余额度%%'
      OR search_text ~* 'RemainQuota[[:space:]]*=[[:space:]]*-'
      OR search_text ILIKE '%%insufficient_user_quota%%'
      OR search_text ILIKE '%%insufficient%%balance%%'
      OR search_text ILIKE '%%INSUFFICIENT_BALANCE%%'
      OR search_text ILIKE '%%not enough credits%%'
      OR search_text ILIKE '%%quota exceeded%%'
    )
),
ranked AS (
  SELECT
    a.id,
    a.name,
    count(*) AS balance_error_count,
    max(be.created_at) AS last_error_at,
    (array_agg(left(replace(coalesce(be.message,''), E'\\n', ' '), 220) ORDER BY be.created_at DESC))[1] AS last_message
  FROM balance_errors be
  JOIN accounts a ON a.id = be.account_id
  WHERE a.deleted_at IS NULL
    AND a.status = 'active'
    AND (
      a.schedulable = true
      OR a.temp_unschedulable_until IS NOT NULL
    )
  GROUP BY a.id, a.name
)
SELECT *
FROM ranked
WHERE balance_error_count >= %(threshold)s
ORDER BY balance_error_count DESC, last_error_at DESC;
"""


GROUPS_SQL = """
SELECT name, platform
FROM groups
WHERE deleted_at IS NULL
ORDER BY platform, sort_order, name;
"""


PLATFORM_OPTIONS_SQL = """
SELECT platform
FROM (
  SELECT platform
  FROM groups
  WHERE deleted_at IS NULL
    AND coalesce(platform, '') <> ''
  UNION
  SELECT platform
  FROM accounts
  WHERE deleted_at IS NULL
    AND coalesce(platform, '') <> ''
  UNION
  SELECT platform
  FROM ops_error_logs
  WHERE created_at >= now() - interval '30 days'
    AND coalesce(platform, '') <> ''
) AS platforms
ORDER BY platform;
"""


ACCOUNT_OPTIONS_SQL = """
SELECT
  id,
  name,
  platform,
  type,
  status,
  schedulable,
  priority
FROM accounts
WHERE deleted_at IS NULL
  AND (%(platform)s = '' OR platform = %(platform)s)
ORDER BY platform, priority, id;
"""
