-- Empty fixture schema for the real app.main idle reference only.
-- The runner executes this solely in its own probe_fixture_<random> database.
-- No production accounts, credentials, history, request bodies or usage exist.
CREATE TABLE accounts (
    id bigint PRIMARY KEY,
    name text NOT NULL DEFAULT '',
    platform text NOT NULL DEFAULT 'openai',
    type text NOT NULL DEFAULT 'oauth',
    status text NOT NULL DEFAULT 'active',
    schedulable boolean NOT NULL DEFAULT true,
    credentials jsonb NOT NULL DEFAULT '{}',
    extra jsonb NOT NULL DEFAULT '{}',
    parent_account_id bigint,
    priority integer NOT NULL DEFAULT 0,
    concurrency integer,
    load_factor numeric,
    proxy_id bigint,
    temp_unschedulable_until timestamptz,
    temp_unschedulable_reason text,
    rate_limited_at timestamptz,
    rate_limit_reset_at timestamptz,
    overload_until timestamptz,
    error_message text,
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
    deleted_at timestamptz
);

CREATE TABLE account_groups (
    account_id bigint NOT NULL REFERENCES accounts(id),
    group_id bigint NOT NULL REFERENCES groups(id),
    PRIMARY KEY (account_id, group_id)
);

CREATE TABLE usage_logs (
    id bigint PRIMARY KEY,
    account_id bigint REFERENCES accounts(id),
    group_id bigint REFERENCES groups(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    model text,
    upstream_model text,
    requested_model text,
    stream boolean NOT NULL DEFAULT false,
    first_token_ms bigint,
    duration_ms bigint,
    input_tokens bigint NOT NULL DEFAULT 0,
    output_tokens bigint NOT NULL DEFAULT 0,
    image_count integer NOT NULL DEFAULT 0,
    image_output_tokens bigint NOT NULL DEFAULT 0,
    inbound_endpoint text
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
    error_type text,
    error_message text,
    upstream_error_message text,
    error_body text,
    upstream_error_detail text,
    upstream_errors jsonb NOT NULL DEFAULT '[]',
    request_id text,
    resolved boolean NOT NULL DEFAULT false
);
