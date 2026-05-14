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
    a.error_message
  FROM accounts a
  JOIN account_groups ag ON ag.account_id = a.id
  JOIN groups g ON g.id = ag.group_id
  WHERE g.name = %(group_name)s
    AND a.deleted_at IS NULL
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
  WHERE (%(range_start)s::timestamptz IS NULL OR e.created_at >= %(range_start)s::timestamptz)
    AND (%(range_end)s::timestamptz IS NULL OR e.created_at < %(range_end)s::timestamptz)
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


TELEGRAM_ERROR_ALERTS_SQL = """
WITH target_logs AS (
  SELECT id
  FROM ops_error_logs
  WHERE id > %(cursor_id)s::bigint
  ORDER BY id ASC
  LIMIT %(limit)s
),
expanded AS (
  SELECT
    e.id AS error_log_id,
    e.created_at,
    e.request_id,
    e.client_request_id,
    e.platform,
    e.model,
    e.requested_model,
    e.upstream_model,
    e.status_code AS final_status_code,
    e.upstream_status_code AS final_upstream_status_code,
    e.error_owner,
    e.error_source,
    e.error_type,
    e.error_phase,
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
    COALESCE(
      CASE WHEN coalesce(x.elem->>'upstream_status_code','') ~ '^[0-9]+$' THEN (x.elem->>'upstream_status_code')::int END,
      e.upstream_status_code,
      e.status_code
    ) AS status_code,
    COALESCE(NULLIF(x.elem->>'kind',''), e.error_type) AS kind,
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
  LEFT JOIN accounts a ON a.id = e.account_id
  LEFT JOIN LATERAL jsonb_array_elements(
    CASE
      WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
      ELSE '[{}]'::jsonb
    END
  ) WITH ORDINALITY AS x(elem, ordinality) ON true
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
  FROM expanded
)
SELECT
  c.error_log_id,
  c.created_at,
  c.request_id,
  c.client_request_id,
  c.platform,
  c.model,
  c.requested_model,
  c.upstream_model,
  c.final_status_code,
  c.final_upstream_status_code,
  c.error_owner,
  c.error_source,
  c.error_type,
  c.error_phase,
  c.attempt_no,
  c.account_id,
  COALESCE(c.account_name, a.name) AS account_name,
  c.status_code,
  c.kind,
  c.category,
  left(replace(coalesce(c.message,''), E'\\n', ' '), 1200) AS message,
  a.schedulable,
  a.temp_unschedulable_until,
  a.temp_unschedulable_reason
FROM classified c
LEFT JOIN accounts a ON a.id = c.account_id
ORDER BY c.error_log_id ASC, c.attempt_no ASC;
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


SCHEDULED_TEST_ACCOUNTS_SQL = """
WITH scoped_accounts AS (
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
    a.updated_at,
    a.error_message,
    a.rate_limited_at,
    a.rate_limit_reset_at,
    a.overload_until,
    a.temp_unschedulable_until,
    a.temp_unschedulable_reason,
    CASE WHEN a.status <> 'active'
      OR a.schedulable = false
      OR a.rate_limited_at IS NOT NULL
      OR a.rate_limit_reset_at IS NOT NULL
      OR a.overload_until IS NOT NULL
      OR a.temp_unschedulable_until IS NOT NULL
      OR coalesce(a.error_message, '') <> ''
    THEN true ELSE false END AS has_recoverable_signal
  FROM accounts a
  LEFT JOIN account_groups ag ON ag.account_id = a.id
  LEFT JOIN groups g ON g.id = ag.group_id
  WHERE a.deleted_at IS NULL
    AND (%(platform)s = '' OR a.platform = %(platform)s)
    AND (%(group_name)s = '' OR g.name = %(group_name)s)
  ORDER BY a.id, ag.priority NULLS LAST, g.name NULLS LAST
),
plans AS (
  SELECT DISTINCT ON (account_id)
    id,
    account_id,
    model_id,
    cron_expression,
    enabled,
    max_results,
    auto_recover,
    last_run_at,
    next_run_at,
    created_at,
    updated_at
  FROM scheduled_test_plans
  ORDER BY account_id, enabled DESC, updated_at DESC, id DESC
),
last_results AS (
  SELECT DISTINCT ON (p.account_id)
    p.account_id,
    r.id AS result_id,
    r.status AS result_status,
    r.response_text AS result_response_text,
    r.error_message AS result_error_message,
    r.latency_ms AS result_latency_ms,
    r.started_at AS result_started_at,
    r.finished_at AS result_finished_at,
    r.created_at AS result_created_at
  FROM scheduled_test_plans p
  JOIN scheduled_test_results r ON r.plan_id = p.id
  ORDER BY p.account_id, r.created_at DESC, r.id DESC
)
SELECT
  sa.*,
  p.id AS plan_id,
  p.model_id AS plan_model_id,
  p.cron_expression AS plan_cron_expression,
  p.enabled AS plan_enabled,
  p.max_results AS plan_max_results,
  p.auto_recover AS plan_auto_recover,
  p.last_run_at AS plan_last_run_at,
  p.next_run_at AS plan_next_run_at,
  p.created_at AS plan_created_at,
  p.updated_at AS plan_updated_at,
  lr.result_id,
  lr.result_status,
  left(replace(coalesce(lr.result_response_text, ''), E'\\n', ' '), 500) AS result_response_text,
  left(replace(coalesce(lr.result_error_message, ''), E'\\n', ' '), 800) AS result_error_message,
  lr.result_latency_ms,
  lr.result_started_at,
  lr.result_finished_at,
  lr.result_created_at
FROM scoped_accounts sa
LEFT JOIN plans p ON p.account_id = sa.id
LEFT JOIN last_results lr ON lr.account_id = sa.id
WHERE %(include_all)s::boolean = true
  OR p.id IS NOT NULL
  OR sa.has_recoverable_signal = true
ORDER BY
  sa.has_recoverable_signal DESC,
  p.enabled DESC NULLS LAST,
  sa.group_priority NULLS LAST,
  sa.account_priority NULLS LAST,
  sa.id;
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


SCHEDULED_TEST_DELETE_SQL = """
DELETE FROM scheduled_test_plans
WHERE id = %(plan_id)s::bigint
RETURNING id, account_id;
"""


SCHEDULED_TEST_RESULTS_SQL = """
SELECT
  r.id,
  r.plan_id,
  p.account_id,
  a.name AS account_name,
  r.status,
  left(replace(coalesce(r.response_text, ''), E'\\n', ' '), 800) AS response_text,
  left(replace(coalesce(r.error_message, ''), E'\\n', ' '), 1200) AS error_message,
  r.latency_ms,
  r.started_at,
  r.finished_at,
  r.created_at
FROM scheduled_test_results r
JOIN scheduled_test_plans p ON p.id = r.plan_id
JOIN accounts a ON a.id = p.account_id
WHERE (%(plan_id)s::bigint IS NULL OR r.plan_id = %(plan_id)s::bigint)
ORDER BY r.created_at DESC, r.id DESC
LIMIT %(limit)s::int;
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
  AND a.updated_at >= r.started_at - interval '5 seconds'
  AND a.updated_at <= COALESCE(r.finished_at, r.created_at) + interval '45 seconds'
ORDER BY r.id ASC
LIMIT %(limit)s::int;
"""
