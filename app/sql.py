# Keep category names and quota terms aligned with app.guard_classifier.
QUALITY_SQL = """
WITH group_accounts AS (
  SELECT DISTINCT ON (a.id)
    a.id,
    a.name,
    a.platform,
    a.type,
    a.status,
    a.schedulable,
    a.priority AS account_priority,
    ag.priority AS group_priority,
    g.name AS group_name,
    a.concurrency,
    a.load_factor,
    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
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
    a.error_message
  FROM accounts a
  JOIN account_groups ag ON ag.account_id = a.id
  JOIN groups g ON g.id = ag.group_id
  WHERE g.name = ANY(%(group_names)s::text[])
    AND a.deleted_at IS NULL
    AND (%(platform)s = '' OR a.platform = %(platform)s)
  ORDER BY a.id, ag.priority NULLS LAST, a.priority NULLS LAST, g.name
),
successes AS (
  SELECT
    account_id,
    count(*) AS success_window,
    COALESCE(sum(output_tokens),0) AS output_tokens_window,
    max(created_at) AS last_success_at,
    round(avg(duration_ms)::numeric, 0) AS avg_duration_ms,
    round(avg(first_token_ms)::numeric, 0) AS avg_first_token_ms,
    round(avg(duration_ms::numeric / NULLIF(output_tokens,0)), 2) AS avg_ms_per_output_token,
    round(COALESCE(sum(total_cost),0), 6) AS usage_total_cost,
    round(COALESCE(sum(actual_cost),0), 6) AS usage_actual_cost,
    COALESCE(sum(
      COALESCE(input_tokens,0)
      + COALESCE(output_tokens,0)
      + COALESCE(cache_creation_tokens,0)
      + COALESCE(cache_read_tokens,0)
      + COALESCE(cache_creation_5m_tokens,0)
      + COALESCE(cache_creation_1h_tokens,0)
      + COALESCE(image_output_tokens,0)
    ),0) AS usage_total_tokens
  FROM usage_logs
  WHERE (%(range_start)s::timestamptz IS NULL OR created_at >= %(range_start)s::timestamptz)
    AND (%(range_end)s::timestamptz IS NULL OR created_at < %(range_end)s::timestamptz)
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
  WHERE (%(range_start)s::timestamptz IS NULL OR e.created_at >= %(range_start)s::timestamptz)
    AND (%(range_end)s::timestamptz IS NULL OR e.created_at < %(range_end)s::timestamptz)
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
        OR search_text ILIKE '%%pre_consume_token_quota_failed%%'
        OR search_text ILIKE '%%token quota is not enough%%'
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
      WHERE category NOT IN ('client_pre_route','client_request','client_bad_request')
    ) AS account_quality_errors_window,
    count(*) FILTER (WHERE category = 'provider_blocked_403') AS blocked_403_window,
    count(*) FILTER (WHERE category = 'provider_balance_or_quota') AS balance_or_quota_window,
    count(*) FILTER (WHERE category = 'provider_rate_limit') AS rate_limit_window,
    count(*) FILTER (WHERE category = 'upstream_unstable_5xx_stream') AS unstable_5xx_stream_window,
    count(*) FILTER (WHERE category = 'client_bad_request') AS client_bad_request_window,
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
  COALESCE(s.output_tokens_window,0) AS output_tokens_window,
  COALESCE(b.account_quality_errors_window,0) AS account_quality_errors_window,
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
  COALESCE(s.success_window,0) AS usage_request_count,
  COALESCE(s.usage_total_cost,0) AS usage_total_cost,
  COALESCE(s.usage_actual_cost,0) AS usage_actual_cost,
  COALESCE(s.usage_total_tokens,0) AS usage_total_tokens,
  le.status_code AS last_error_status,
  le.kind AS last_error_kind,
  le.category AS last_error_category,
  le.message AS last_error_message,
  s.avg_duration_ms,
  s.avg_first_token_ms,
  s.avg_ms_per_output_token
FROM group_accounts ga
LEFT JOIN successes s ON s.account_id = ga.id
LEFT JOIN by_account b ON b.account_id = ga.id
LEFT JOIN last_error le ON le.account_id = ga.id
ORDER BY ga.group_priority, ga.account_priority, ga.id;
"""


QUALITY_SQL_COMPAT_NO_LOAD_FACTOR = QUALITY_SQL.replace(
    "    a.load_factor,\n    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
    "    NULL::integer AS load_factor,\n    COALESCE(NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
)


SPEED_SQL = QUALITY_SQL.replace(
    "    a.type,\n    a.status,\n",
    """    a.type,
    jsonb_strip_nulls(jsonb_build_object(
      'plan_type', a.credentials->>'plan_type',
      'chatgpt_plan_type', a.credentials->>'chatgpt_plan_type'
    )) AS credentials,
    jsonb_strip_nulls(jsonb_build_object(
      'plan_type', a.extra->>'plan_type',
      'codex_usage_updated_at', a.extra->>'codex_usage_updated_at',
      'codex_5h_used_percent', a.extra->'codex_5h_used_percent',
      'codex_5h_reset_at', a.extra->>'codex_5h_reset_at',
      'codex_5h_reset_after_seconds', a.extra->'codex_5h_reset_after_seconds',
      'codex_5h_window_minutes', a.extra->'codex_5h_window_minutes',
      'codex_7d_used_percent', a.extra->'codex_7d_used_percent',
      'codex_7d_reset_at', a.extra->>'codex_7d_reset_at',
      'codex_7d_reset_after_seconds', a.extra->'codex_7d_reset_after_seconds',
      'codex_7d_window_minutes', a.extra->'codex_7d_window_minutes'
    )) AS extra,
    a.status,
""",
    1,
)


SPEED_SQL_COMPAT_NO_LOAD_FACTOR = SPEED_SQL.replace(
    "    a.load_factor,\n    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
    "    NULL::integer AS load_factor,\n    COALESCE(NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
)


QUALITY_ALL_ACCOUNTS_SQL = QUALITY_SQL.replace(
    """WITH group_accounts AS (
  SELECT DISTINCT ON (a.id)
    a.id,
    a.name,
    a.platform,
    a.type,
    a.status,
    a.schedulable,
    a.priority AS account_priority,
    ag.priority AS group_priority,
    g.name AS group_name,
    a.concurrency,
    a.load_factor,
    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
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
    a.error_message
  FROM accounts a
  JOIN account_groups ag ON ag.account_id = a.id
  JOIN groups g ON g.id = ag.group_id
  WHERE g.name = ANY(%(group_names)s::text[])
    AND a.deleted_at IS NULL
    AND (%(platform)s = '' OR a.platform = %(platform)s)
  ORDER BY a.id, ag.priority NULLS LAST, a.priority NULLS LAST, g.name
),""",
    """WITH group_accounts AS (
  SELECT DISTINCT ON (a.id)
    a.id,
    a.name,
    a.platform,
    a.type,
    a.status,
    a.schedulable,
    a.priority AS account_priority,
    ag.priority AS group_priority,
    g.name AS group_name,
    a.concurrency,
    a.load_factor,
    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
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
    a.error_message
  FROM accounts a
  LEFT JOIN account_groups ag ON ag.account_id = a.id
  LEFT JOIN groups g ON g.id = ag.group_id
  WHERE a.deleted_at IS NULL
  ORDER BY a.id, ag.priority NULLS LAST, a.priority NULLS LAST, g.name NULLS LAST
),""",
)
QUALITY_ALL_ACCOUNTS_SQL_COMPAT_NO_LOAD_FACTOR = QUALITY_ALL_ACCOUNTS_SQL.replace(
    "    a.load_factor,\n    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
    "    NULL::integer AS load_factor,\n    COALESCE(NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
)


GUARD_QUEUE_SQL = QUALITY_SQL.replace(
    """WITH group_accounts AS (
  SELECT DISTINCT ON (a.id)
    a.id,
    a.name,
    a.platform,
    a.type,
    a.status,
    a.schedulable,
    a.priority AS account_priority,
    ag.priority AS group_priority,
    g.name AS group_name,
    a.concurrency,
    a.load_factor,
    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
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
    a.error_message
  FROM accounts a
  JOIN account_groups ag ON ag.account_id = a.id
  JOIN groups g ON g.id = ag.group_id
  WHERE g.name = ANY(%(group_names)s::text[])
    AND a.deleted_at IS NULL
    AND (%(platform)s = '' OR a.platform = %(platform)s)
  ORDER BY a.id, ag.priority NULLS LAST, a.priority NULLS LAST, g.name
),""",
    """WITH group_accounts AS (
  SELECT
    a.id,
    ag.group_id,
    COALESCE(g.sort_order, 999999) AS group_sort_order,
    a.name,
    a.platform,
    a.type,
    a.status,
    a.schedulable,
    a.priority AS account_priority,
    ag.priority AS group_priority,
    g.name AS group_name,
    a.concurrency,
    a.load_factor,
    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
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
    a.error_message
  FROM account_groups ag
  JOIN accounts a ON a.id = ag.account_id
  JOIN groups g ON g.id = ag.group_id
  WHERE a.deleted_at IS NULL
    AND g.deleted_at IS NULL
  ORDER BY g.platform NULLS LAST, g.sort_order NULLS LAST, g.name, ag.priority NULLS LAST, a.priority NULLS LAST, a.id
),""",
)
GUARD_QUEUE_SQL = GUARD_QUEUE_SQL.replace(
    "ORDER BY ga.group_priority, ga.account_priority, ga.id;",
    "ORDER BY ga.platform NULLS LAST, ga.group_sort_order NULLS LAST, ga.group_name NULLS LAST, ga.group_priority NULLS LAST, ga.account_priority NULLS LAST, ga.id;",
)


GUARD_QUEUE_SQL_COMPAT_NO_LOAD_FACTOR = GUARD_QUEUE_SQL.replace(
    "    a.load_factor,\n    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
    "    NULL::integer AS load_factor,\n    COALESCE(NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
)


# Keep category names and quota terms aligned with app.guard_classifier.
GUARD_BALANCE_CANDIDATES_SQL = """
WITH target_logs AS (
  SELECT id
  FROM ops_error_logs
  WHERE created_at >= now() - (%(max_age_hours)s::text || ' hours')::interval
),
raw_error_attempts AS (
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
  FROM target_logs t
  JOIN ops_error_logs e ON e.id = t.id
  LEFT JOIN LATERAL jsonb_array_elements(
    CASE
      WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
      ELSE '[{}]'::jsonb
    END
  ) AS x(elem) ON true
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
      OR search_text ILIKE '%%pre_consume_token_quota_failed%%'
      OR search_text ILIKE '%%token quota is not enough%%'
      OR search_text ILIKE '%%quota exceeded%%'
    )
),
ranked AS (
  SELECT
    a.id,
    a.name,
    a.type AS account_type,
    count(*) AS balance_error_count,
    max(be.created_at) AS last_error_at,
    (array_agg(left(replace(coalesce(be.message,''), E'\\n', ' '), 220) ORDER BY be.created_at DESC))[1] AS last_message,
    (
      a.schedulable = false
      AND a.temp_unschedulable_until IS NULL
      AND coalesce(a.temp_unschedulable_reason, '') ILIKE 'auto guard:%%balance/quota%%'
    ) AS already_auto_guarded
  FROM balance_errors be
  JOIN accounts a ON a.id = be.account_id
  WHERE a.deleted_at IS NULL
    AND a.status = 'active'
    AND lower(coalesce(a.type, '')) <> 'oauth'
    AND (
      a.schedulable = false
      OR a.temp_unschedulable_until IS NOT NULL
      OR a.rate_limited_at IS NOT NULL
      OR a.rate_limit_reset_at IS NOT NULL
      OR a.overload_until IS NOT NULL
      OR coalesce(a.error_message, '') <> ''
    )
  GROUP BY a.id, a.name, a.type, a.schedulable, a.temp_unschedulable_until, a.temp_unschedulable_reason
)
SELECT *
FROM ranked
WHERE balance_error_count >= %(threshold)s
  AND already_auto_guarded = false
ORDER BY balance_error_count DESC, last_error_at DESC;
"""


GUARD_ERROR_EVENTS_SQL = """
WITH target_logs AS (
  SELECT id
  FROM ops_error_logs
  WHERE id > %(cursor_id)s::bigint
  ORDER BY id ASC
  LIMIT %(limit)s::int
)
SELECT
  e.id AS error_log_id,
  e.created_at,
  e.request_id,
  e.client_request_id,
  e.platform,
  e.model,
  x.ordinality AS attempt_no,
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
  a.priority AS account_priority,
  a.load_factor,
  a.concurrency,
  COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
  a.type AS account_type,
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
FROM target_logs t
JOIN ops_error_logs e ON e.id = t.id
LEFT JOIN LATERAL jsonb_array_elements(
  CASE
    WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
    ELSE '[{}]'::jsonb
  END
) WITH ORDINALITY AS x(elem, ordinality) ON true
LEFT JOIN accounts a ON a.id = COALESCE(
  CASE WHEN coalesce(x.elem->>'account_id','') ~ '^[0-9]+$' THEN (x.elem->>'account_id')::bigint END,
  e.account_id
)
ORDER BY e.id ASC, x.ordinality ASC;
"""


GUARD_ERROR_EVENTS_SQL_COMPAT_NO_LOAD_FACTOR = GUARD_ERROR_EVENTS_SQL.replace(
    "  a.load_factor,\n  a.concurrency,\n  COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
    "  NULL::integer AS load_factor,\n  a.concurrency,\n  COALESCE(NULLIF(a.concurrency, 0), 1) AS effective_load_factor,\n",
)


GUARD_SUCCESS_EVENTS_SQL = """
SELECT
  u.account_id,
  a.type AS account_type,
  max(u.created_at) AS success_created_at,
  ('success:' || u.account_id::text || ':' || max(u.created_at)::text) AS success_event_key,
  count(*) AS success_count,
  COALESCE(sum(u.output_tokens), 0) AS output_tokens,
  round(avg(u.duration_ms)::numeric, 0) AS avg_duration_ms,
  round(avg(u.first_token_ms)::numeric, 0) AS avg_first_token_ms
FROM usage_logs u
LEFT JOIN accounts a ON a.id = u.account_id
WHERE u.account_id IS NOT NULL
  AND (%(cursor_created_at)s = '' OR u.created_at > %(cursor_created_at)s::timestamptz)
GROUP BY u.account_id, a.type
ORDER BY max(u.created_at) ASC
LIMIT %(limit)s::int;
"""


ACCOUNT_ROUTING_CAPABILITY_SQL = """
SELECT
  EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'accounts'
      AND column_name = 'priority'
  ) AS account_priority_column_exists,
  EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'accounts'
      AND column_name = 'load_factor'
  ) AS account_load_factor_column_exists,
  EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'account_groups'
      AND column_name = 'priority'
  ) AS account_group_priority_column_exists;
"""


GUARD_ACCOUNT_ROUTING_UPDATE_SQL = """
UPDATE accounts
SET priority = %(priority)s::int,
    load_factor = CASE
      WHEN %(load_factor)s::int IS NULL OR %(load_factor)s::int <= 0 THEN NULL
      ELSE %(load_factor)s::int
    END,
    updated_at = now()
WHERE id = %(account_id)s::bigint
  AND deleted_at IS NULL
RETURNING id, name, priority AS account_priority, load_factor, concurrency, updated_at;
"""


GUARD_ACCOUNT_PRIORITY_UPDATE_SQL = """
UPDATE accounts
SET priority = %(priority)s::int,
    updated_at = now()
WHERE id = %(account_id)s::bigint
  AND deleted_at IS NULL
RETURNING id, name, priority AS account_priority, concurrency, updated_at;
"""


GUARD_ACCOUNT_LOAD_FACTOR_UPDATE_SQL = """
UPDATE accounts
SET load_factor = CASE
      WHEN %(load_factor)s::int IS NULL OR %(load_factor)s::int <= 0 THEN NULL
      ELSE %(load_factor)s::int
    END,
    updated_at = now()
WHERE id = %(account_id)s::bigint
  AND deleted_at IS NULL
RETURNING id, name, load_factor, concurrency, updated_at;
"""


GUARD_ACCOUNT_GROUP_PRIORITY_UPDATE_SQL = """
UPDATE account_groups ag
SET priority = %(group_priority)s::int
FROM groups g
WHERE g.id = ag.group_id
  AND ag.account_id = %(account_id)s::bigint
  AND (%(group_id)s::bigint IS NULL OR ag.group_id = %(group_id)s::bigint)
  AND (%(group_name)s::text = '' OR g.name = %(group_name)s::text)
RETURNING ag.account_id, ag.group_id, g.name AS group_name, ag.priority AS group_priority;
"""


GROUPS_SQL = """
SELECT id, name, platform
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


SCHEDULED_TEST_CAPABILITY_SQL = """
SELECT
  EXISTS (
    SELECT 1
    FROM information_schema.tables
    WHERE table_schema = 'public'
      AND table_name = 'scheduled_test_plans'
  ) AS plans_table_exists,
  EXISTS (
    SELECT 1
    FROM information_schema.tables
    WHERE table_schema = 'public'
      AND table_name = 'scheduled_test_results'
  ) AS results_table_exists,
  EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'scheduled_test_plans'
      AND column_name = 'auto_recover'
  ) AS auto_recover_column_exists;
"""


SCHEDULED_TEST_UPSERT_SQL = """
WITH existing AS (
  SELECT id
  FROM scheduled_test_plans
  WHERE account_id = %(account_id)s::bigint
  ORDER BY enabled DESC, updated_at DESC, id DESC
  LIMIT 1
),
updated AS (
  UPDATE scheduled_test_plans p
  SET model_id = %(model_id)s,
      cron_expression = %(cron_expression)s,
      enabled = %(enabled)s::boolean,
      max_results = %(max_results)s::int,
      auto_recover = %(auto_recover)s::boolean,
      next_run_at = %(next_run_at)s::timestamptz,
      updated_at = now()
  FROM existing e
  WHERE p.id = e.id
  RETURNING p.id, p.account_id, p.model_id, p.cron_expression, p.enabled, p.max_results, p.auto_recover, p.last_run_at, p.next_run_at, p.created_at, p.updated_at
),
inserted AS (
  INSERT INTO scheduled_test_plans (account_id, model_id, cron_expression, enabled, max_results, auto_recover, next_run_at, created_at, updated_at)
  SELECT
    %(account_id)s::bigint,
    %(model_id)s,
    %(cron_expression)s,
    %(enabled)s::boolean,
    %(max_results)s::int,
    %(auto_recover)s::boolean,
    %(next_run_at)s::timestamptz,
    now(),
    now()
  WHERE NOT EXISTS (SELECT 1 FROM updated)
  RETURNING id, account_id, model_id, cron_expression, enabled, max_results, auto_recover, last_run_at, next_run_at, created_at, updated_at
)
SELECT *
FROM updated
UNION ALL
SELECT *
FROM inserted;
"""


SCHEDULED_TEST_DELETE_ENDLESS_SQL = """
DELETE FROM scheduled_test_plans
WHERE account_id = %(account_id)s::bigint
  AND cron_expression = '* * * * *'
  AND auto_recover = true
  AND (%(plan_id)s::bigint IS NULL OR id = %(plan_id)s::bigint)
RETURNING id, account_id;
"""


SCHEDULED_TEST_ENDLESS_PLANS_SQL = """
SELECT
  id AS plan_id,
  account_id,
  cron_expression,
  enabled,
  auto_recover
FROM scheduled_test_plans
WHERE cron_expression = '* * * * *'
  AND auto_recover = true
ORDER BY id ASC;
"""


SCHEDULED_TEST_RECOVERY_ALERTS_SQL = """
SELECT
  r.id AS result_id,
  r.plan_id,
  r.status AS result_status,
  r.latency_ms,
  r.started_at,
  r.finished_at,
  r.created_at,
  p.account_id,
  p.model_id,
  p.cron_expression,
  p.auto_recover,
  a.id,
  a.name AS account_name,
  a.platform,
  a.type,
  a.status AS account_status,
  a.schedulable,
  a.updated_at AS account_updated_at,
  a.error_message,
  a.rate_limited_at,
  a.rate_limit_reset_at,
  a.overload_until,
  a.temp_unschedulable_until,
  a.temp_unschedulable_reason
FROM scheduled_test_results r
JOIN scheduled_test_plans p ON p.id = r.plan_id
JOIN accounts a ON a.id = p.account_id
WHERE r.id > %(cursor_id)s::bigint
  AND p.auto_recover = true
  AND r.status = 'success'
  AND r.created_at <= now() - interval '5 seconds'
ORDER BY r.id ASC
LIMIT %(limit)s::int;
"""
