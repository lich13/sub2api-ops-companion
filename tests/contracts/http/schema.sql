-- Synthetic PostgreSQL schema for the real app.main HTTP contract runner.
-- This uses the application tables and SQL projections; it does not create or
-- read the connection-probe prototype's probe_accounts table.

CREATE TABLE accounts (
    id bigint PRIMARY KEY,
    name text NOT NULL DEFAULT '',
    platform text NOT NULL DEFAULT 'openai',
    type text NOT NULL DEFAULT 'oauth',
    status text NOT NULL DEFAULT 'active',
    schedulable boolean NOT NULL DEFAULT false,
    credentials jsonb NOT NULL DEFAULT '{}',
    extra jsonb NOT NULL DEFAULT '{}',
    parent_account_id bigint,
    priority integer NOT NULL DEFAULT 0,
    concurrency integer,
    load_factor integer,
    proxy_id bigint,
    temp_unschedulable_until timestamptz,
    temp_unschedulable_reason text,
    rate_limited_at timestamptz,
    rate_limit_reset_at timestamptz,
    overload_until timestamptz,
    error_message text,
    error_code text,
    error_status_code integer,
    expires_at timestamptz,
    auto_pause_on_expired boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

CREATE TABLE groups (
    id bigint PRIMARY KEY,
    name text NOT NULL DEFAULT '',
    platform text NOT NULL DEFAULT 'openai',
    sort_order integer NOT NULL DEFAULT 0,
    model_allowlist jsonb NOT NULL DEFAULT '{"enabled":false,"models":[]}',
    codex_models_manifest_config jsonb NOT NULL DEFAULT '{}',
    max_reasoning_effort text,
    max_reasoning_effort_over_limit text,
    reasoning_effort_mappings jsonb NOT NULL DEFAULT '{}',
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

CREATE TABLE account_groups (
    account_id bigint NOT NULL REFERENCES accounts(id),
    group_id bigint NOT NULL REFERENCES groups(id),
    PRIMARY KEY (account_id, group_id)
);

CREATE TABLE users (
    id bigint PRIMARY KEY,
    username text NOT NULL,
    email text NOT NULL,
    status text NOT NULL DEFAULT 'active',
    deleted_at timestamptz,
    password text,
    credentials jsonb NOT NULL DEFAULT '{}'
);

CREATE TABLE api_keys (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    user_id bigint REFERENCES users(id),
    group_id bigint REFERENCES groups(id),
    status text NOT NULL DEFAULT 'active',
    key text NOT NULL,
    token text,
    deleted_at timestamptz
);

CREATE TABLE settings (
    key text PRIMARY KEY,
    value text NOT NULL DEFAULT ''
);

CREATE TABLE proxies (
    id bigint PRIMARY KEY,
    protocol text NOT NULL DEFAULT 'http',
    host text NOT NULL DEFAULT '127.0.0.1',
    port integer NOT NULL DEFAULT 9,
    username text NOT NULL DEFAULT '',
    password text NOT NULL DEFAULT '',
    deleted_at timestamptz
);

CREATE TABLE composite_model_routes (
    id bigint PRIMARY KEY,
    group_id bigint NOT NULL REFERENCES groups(id),
    public_model text NOT NULL,
    match_type text NOT NULL DEFAULT 'exact',
    target_platform text NOT NULL,
    upstream_model text NOT NULL,
    endpoint text NOT NULL DEFAULT '',
    priority integer NOT NULL DEFAULT 0,
    enabled boolean NOT NULL DEFAULT true
);

CREATE TABLE scheduled_test_plans (
    id bigint PRIMARY KEY,
    account_id bigint REFERENCES accounts(id),
    cron_expression text NOT NULL,
    auto_recover boolean NOT NULL DEFAULT false
);

CREATE TABLE usage_logs (
    id bigint PRIMARY KEY,
    user_id bigint REFERENCES users(id),
    api_key_id bigint REFERENCES api_keys(id),
    account_id bigint REFERENCES accounts(id),
    group_id bigint REFERENCES groups(id),
    request_id text,
    upstream_request_id text,
    model text,
    requested_model text,
    upstream_model text,
    upstream_response_model text,
    model_mapping_chain text,
    upstream_model_mismatch boolean NOT NULL DEFAULT false,
    reasoning_effort text,
    requested_reasoning_effort text,
    request_type integer,
    stream boolean NOT NULL DEFAULT false,
    openai_ws_mode boolean NOT NULL DEFAULT false,
    input_tokens bigint NOT NULL DEFAULT 0,
    output_tokens bigint NOT NULL DEFAULT 0,
    cache_creation_tokens bigint NOT NULL DEFAULT 0,
    cache_read_tokens bigint NOT NULL DEFAULT 0,
    cache_creation_5m_tokens bigint NOT NULL DEFAULT 0,
    cache_creation_1h_tokens bigint NOT NULL DEFAULT 0,
    input_cost numeric(24, 10),
    output_cost numeric(24, 10),
    cache_creation_cost numeric(24, 10),
    cache_read_cost numeric(24, 10),
    total_cost numeric(24, 10),
    actual_cost numeric(24, 10),
    account_stats_cost numeric(24, 10),
    account_rate_multiplier numeric(24, 10),
    rate_multiplier numeric(24, 10),
    first_token_ms bigint,
    duration_ms bigint,
    created_at timestamptz NOT NULL DEFAULT now(),
    user_agent text,
    ip_address text,
    inbound_endpoint text,
    upstream_endpoint text,
    billing_type text,
    billing_mode text,
    service_tier text,
    image_count integer NOT NULL DEFAULT 0,
    image_output_tokens bigint NOT NULL DEFAULT 0,
    image_output_cost numeric(24, 10),
    image_input_tokens bigint NOT NULL DEFAULT 0,
    image_input_cost numeric(24, 10),
    video_count integer NOT NULL DEFAULT 0,
    video_duration_seconds numeric(24, 10),
    video_resolution text,
    request_headers jsonb,
    response_headers jsonb,
    request_body text,
    response_body text,
    credentials jsonb
);

CREATE TABLE ops_error_logs (
    id bigint PRIMARY KEY,
    account_id bigint REFERENCES accounts(id),
    group_id bigint REFERENCES groups(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    platform text,
    error_owner text,
    error_phase text,
    error_source text,
    stream boolean NOT NULL DEFAULT false,
    model text,
    requested_model text,
    upstream_model text,
    status_code integer,
    upstream_status_code integer,
    provider_error_code text,
    provider_error_type text,
    network_error_type text,
    error_type text,
    error_message text,
    upstream_error_message text,
    error_body text,
    upstream_error_detail text,
    upstream_errors jsonb NOT NULL DEFAULT '[]',
    request_id text,
    client_request_id text,
    resolved boolean NOT NULL DEFAULT false
);

-- Every row below is synthetic. Secrets and private bodies are sentinels used
-- only to assert that public DTOs redact them.
INSERT INTO accounts (
    id, name, platform, type, status, schedulable, credentials, extra, priority,
    concurrency, load_factor, updated_at
) VALUES (
    4001, 'fixture-account-040', 'openai', 'oauth', 'active', false,
    '{"access_token":"fixture-account-token-040","model_mapping":{"fixture-requested-model":"fixture-native-model"}}',
    '{"quota_daily_limit":100,"quota_daily_used":12}', 3, 2, 1, now()
);

INSERT INTO groups (
    id, name, platform, sort_order, model_allowlist, updated_at
) VALUES (
    4301, 'fixture-group-040', 'openai', 1,
    '{"enabled":false,"models":[]}', now()
);

INSERT INTO account_groups (account_id, group_id) VALUES (4001, 4301);

INSERT INTO users (id, username, email, status, password, credentials)
VALUES (4401, 'fixture-user-040', 'contract-040@example.invalid', 'active',
        'fixture-password-not-for-response', '{"token":"fixture-user-token-not-for-response"}');

INSERT INTO api_keys (id, name, user_id, group_id, status, key, token)
VALUES (4501, 'fixture-api-key-name-040', 4401, 4301, 'active',
        'fixture-client-key-040', 'fixture-client-token-not-for-response');

INSERT INTO usage_logs (
    id, user_id, api_key_id, account_id, group_id, request_id, upstream_request_id,
    model, requested_model, upstream_model, upstream_response_model, model_mapping_chain,
    request_type, input_tokens, output_tokens, input_cost, output_cost, total_cost,
    actual_cost, account_stats_cost, account_rate_multiplier, first_token_ms, duration_ms,
    created_at, user_agent, ip_address, inbound_endpoint, upstream_endpoint,
    request_body, response_body, request_headers, response_headers, credentials
) VALUES (
    91001, 4401, 4501, 4001, 4301, 'fixture-request-id-040', 'fixture-upstream-request-id-040',
    'fixture-requested-model', 'fixture-requested-model', 'fixture-native-model', 'fixture-native-model',
    'fixture-requested-model -> fixture-native-model', 1, 120, 45, 0.0100000000, 0.0200000000,
    0.0300000000, 0.0250000000, 0.0250000000, 1.0000000000, 25, 150,
    now() - interval '1 hour', 'fixture-agent', '127.0.0.1', '/v1/responses', '/v1/responses',
    'fixture-private-request-body', 'fixture-private-response-body',
    '{"authorization":"Bearer fixture-header-token"}', '{"set-cookie":"fixture-cookie"}',
    '{"access_token":"fixture-usage-token"}'
);

INSERT INTO ops_error_logs (
    id, account_id, group_id, created_at, platform, error_owner, error_phase, error_source,
    model, requested_model, upstream_model, status_code, upstream_status_code,
    provider_error_code, provider_error_type, error_type, error_message, upstream_error_message,
    error_body, upstream_error_detail, upstream_errors, request_id, client_request_id, resolved
) VALUES (
    92001, 4001, 4301, now() - interval '30 minutes', 'openai', 'provider', 'upstream', 'fixture',
    'fixture-requested-model', 'fixture-requested-model', 'fixture-native-model', 401, 401,
    'fixture_auth_failure', 'authentication_error', 'auth', 'fixture auth failure',
    'fixture upstream auth failure',
    '{"error":{"code":"fixture_auth_failure","message":"Bearer fixture-error-token"},"request_body":"fixture-error-request-body"}',
    '{"error":{"code":"fixture_auth_failure","message":"Bearer fixture-error-token"},"authorization":"fixture-error-header"}',
    '[{"account_id":4001,"kind":"auth","message":"fixture auth failure"}]',
    'fixture-error-request-id-040', 'fixture-client-request-id-040', false
);
