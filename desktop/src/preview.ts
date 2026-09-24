// Development-only, deterministic fixtures. The native build never uses this transport.
import type { ViewState, Account, Config } from "./types";
const now = new Date().toISOString(),
  before = new Date(Date.now() - 7 * 60e3).toISOString();
const accounts: Account[] = [
  {
    id: 101,
    name: "Codex · 主力",
    platform: "openai",
    type: "oauth",
    status: "active",
    schedulable: true,
    available: true,
    group_ids: [1],
    blockers: [],
    managed: false,
    version: "a".repeat(64),
    last_success_at: now,
    last_error_at: null,
    last_error_id: null,
    last_error_code: null,
    last_error_status: null,
    error_message: "",
    success_after_error: false,
    usage_windows: [
      {
        key: "5h",
        label: "5h",
        used_percent: 38,
        reset_at: "2026-09-25T05:00:00+08:00",
        observed_at: now,
        status: "known",
        source: "passive",
      },
      {
        key: "7d",
        label: "7d",
        used_percent: 81,
        reset_at: "2026-09-28T05:00:00+08:00",
        observed_at: now,
        status: "known",
        source: "passive",
      },
    ],
  },
  {
    id: 102,
    name: "Codex · 备用账号 · 用于验证长名称的显示与调度开关",
    platform: "openai",
    type: "oauth",
    status: "error",
    schedulable: true,
    available: false,
    group_ids: [1],
    blockers: [{ code: "reauth", label: "需要重新认证" }],
    managed: false,
    version: "b".repeat(64),
    last_success_at: before,
    last_error_at: now,
    last_error_id: 9001,
    last_error_code: "invalid_grant",
    last_error_status: 401,
    error_message: "Authentication token expired. Please sign in again.",
    success_after_error: false,
    usage_windows: [
      {
        key: "7d",
        label: "7d",
        used_percent: null,
        reset_at: null,
        observed_at: before,
        status: "error",
        source: "passive",
      },
    ],
  },
  {
    id: 201,
    name: "Grok · Super",
    platform: "grok",
    type: "oauth",
    status: "active",
    schedulable: true,
    available: true,
    group_ids: [2],
    blockers: [],
    managed: false,
    version: "c".repeat(64),
    last_success_at: now,
    last_error_at: before,
    last_error_id: 9002,
    last_error_code: "rate_limit_exceeded",
    last_error_status: 429,
    error_message: "Too many requests.",
    success_after_error: true,
    usage_windows: [
      {
        key: "requests",
        label: "请求",
        used_percent: 18,
        used: 180,
        limit: 1000,
        remaining: 820,
        reset_at: "2026-09-28T05:00:00+08:00",
        observed_at: now,
        status: "known",
        source: "upstream_headers",
      },
      {
        key: "tokens",
        label: "Token",
        used_percent: 45,
        used: 450000,
        limit: 1000000,
        remaining: 550000,
        reset_at: "2026-10-01T00:00:00+08:00",
        observed_at: before,
        status: "stale",
        source: "upstream_headers",
      },
    ],
  },
  {
    id: 389,
    name: "OpenAI · 按量备用",
    platform: "openai",
    type: "apikey",
    status: "active",
    schedulable: false,
    available: false,
    group_ids: [1],
    blockers: [{ code: "disabled", label: "调度已关闭" }],
    managed: true,
    version: "d".repeat(64),
    last_success_at: null,
    last_error_at: null,
    last_error_id: null,
    last_error_code: null,
    last_error_status: null,
    error_message: "",
    success_after_error: false,
    usage_windows: [],
  },
];
const stats = {
  requests: 313,
  tokens: 46900000,
  cost: 76.8,
  standard_cost: 76.8,
  user_cost: 76.8,
};
for (const account of accounts) {
  account.usage = {
    branch:
      account.type === "apikey"
        ? "apikey"
        : account.platform === "grok"
          ? "grok_paid"
          : "openai_oauth",
    windows: account.usage_windows,
    today:
      account.type === "apikey"
        ? { requests: 0, tokens: 0, cost: 0, standard_cost: 0, user_cost: 0 }
        : null,
    actions:
      account.type === "apikey"
        ? []
        : account.platform === "grok"
          ? ["probe_quota"]
          : ["query_usage", "query_reset_credits", "reset_quota"],
    reset_credits:
      account.platform === "openai" && account.type === "oauth"
        ? {
            available: 1,
            expires_at: ["2026-10-23T02:52:00+08:00"],
            observed_at: now,
          }
        : null,
  };
  for (const window of account.usage.windows) {
    window.stats = stats;
    window.color =
      window.label === "7d" && account.platform === "openai"
        ? "emerald"
        : "indigo";
    if (account.platform === "grok") {
      window.label = window.key === "requests" ? "7d" : "30d";
      window.source = "billing_probe";
    }
    if (account.platform === "openai" && window.label === "7d")
      window.estimated_total_cost = 94.81;
  }
}
accounts.push({
  ...accounts[3],
  id: 390,
  name: "Grok · 按量 Key",
  platform: "grok",
  managed: false,
  usage: {
    branch: "apikey",
    windows: [
      {
        key: "daily",
        label: "1d",
        used_percent: 75,
        reset_at: now,
        observed_at: now,
        status: "known",
        source: "configured_quota",
        color: "indigo",
      },
    ],
    today: stats,
    actions: [],
    reset_credits: null,
  },
});
accounts.push({
  ...accounts[2],
  id: 391,
  name: "Grok · Free",
  last_error_id: null,
  last_error_at: null,
  usage: {
    branch: "grok_free",
    windows: [
      {
        key: "grok_24h",
        label: "24h",
        used_percent: 90,
        reset_at: null,
        observed_at: now,
        status: "known",
        source: "local_24h",
        color: "emerald",
        stats,
      },
    ],
    today: null,
    actions: ["probe_quota"],
    reset_credits: null,
  },
});
let state: ViewState = {
  connected: true,
  online: true,
  error: "",
  preferences: {
    base_url: "https://demo.example/sub2ops",
    favorites: [1, 2],
    pinned: false,
    launch_at_login: false,
  },
  snapshot: {
    observed_at: now,
    accounts,
    groups: [
      {
        id: 1,
        name: "Codex",
        platform: "openai",
        account_id: 101,
        account_name: accounts[0].name,
        model: "gpt-6-astra",
        upstream_model: "gpt-6-astra",
        upstream_response_model: "gpt-6-astra",
        called_at: now,
        recent_accounts: [0, 1, 3].map((index, rank) => ({
          log_id: 100 - rank,
          account_id: accounts[index].id,
          account_name: accounts[index].name,
          model: "gpt-6-astra",
          upstream_model: "gpt-6-astra",
          called_at: rank ? before : now,
        })),
      },
      {
        id: 2,
        name: "Grok",
        platform: "grok",
        account_id: 201,
        account_name: accounts[2].name,
        model: "grok-4.6",
        upstream_model: "grok-4.6",
        upstream_response_model: "grok-4.6",
        called_at: now,
      },
      {
        id: 3,
        name: "实验分组",
        platform: "openai",
        account_id: null,
        account_name: "",
        model: "",
        upstream_model: "",
        upstream_response_model: "",
        called_at: null,
      },
    ],
    errors: [
      {
        id: 9001,
        account_id: 102,
        account_name: accounts[1].name,
        group_id: 1,
        group_name: "Codex",
        created_at: now,
        model: "gpt-6-sol",
        requested_model: "gpt-6-sol",
        upstream_model: "gpt-6-sol",
        status_code: 401,
        upstream_status_code: 401,
        provider_error_code: "invalid_grant",
        message: accounts[1].error_message,
        request_id: "req-example-9001",
        resolved: false,
      },
      {
        id: 9002,
        account_id: 201,
        account_name: accounts[2].name,
        group_id: 2,
        group_name: "Grok",
        created_at: before,
        model: "grok-4.6",
        requested_model: "grok-4.6",
        upstream_model: "grok-4.6",
        status_code: 429,
        upstream_status_code: 429,
        provider_error_code: "rate_limit_exceeded",
        message: "Too many requests.",
        request_id: "req-example-9002",
        resolved: false,
      },
    ],
    incidents: [
      {
        account_id: 102,
        account_name: accounts[1].name,
        platform: "openai",
        account_type: "oauth",
        requested_model: "gpt-6-astra",
        upstream_model: "gpt-6-astra",
        response_model: "gpt-5.6-luna",
        status: "confirmed",
        action: "历史仅告警",
        reason: "OpenAI 档位下降",
        first_at: before,
        latest_at: now,
        count: 3,
        history: true,
      },
    ],
  },
};
const config: Config = {
  oauth: {
    revision: "1".repeat(64),
    oauth_recovery_monitor_enabled: true,
    oauth_daily_test_enabled: true,
    oauth_daily_test_time: "05:00",
    oauth_usage_refresh_concurrency: 4,
    oauth_recovery_test_concurrency: 2,
    oauth_early_probe_batch_size: 8,
    oauth_7d_probe_interval_seconds: 3600,
    oauth_recovery_test_model_id: "gpt-5.6-luna",
  },
  bark: { revision: "2".repeat(64), enabled: true, device_key_set: true },
  telegram: {
    revision: "3".repeat(64),
    configured: true,
    bot_token_set: true,
    pairing_code: "DEMO-CODE",
    paired_user_count: 1,
    paired_chat_count: 1,
  },
  key_fallback: {
    revision: "4".repeat(64),
    openai_enabled: false,
    grok_enabled: false,
    managed_account_ids: [389],
  },
  model_guard: {
    revision: "5".repeat(64),
    openai_enabled: true,
    grok_enabled: true,
    auto_remove: true,
  },
};
const listeners = new Set<(s: ViewState) => void>();
function emit() {
  for (const cb of listeners) cb(structuredClone(state));
}
export function subscribe(cb: (s: ViewState) => void) {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}
export async function run(
  name: string,
  args: Record<string, unknown>,
): Promise<unknown> {
  if (name === "get_state") return structuredClone(state);
  if (name === "refresh") {
    emit();
    return;
  }
  if (name === "preferences") {
    state.preferences = { ...state.preferences, ...args };
    emit();
    return;
  }
  if (name === "connect") {
    state.connected = true;
    state.online = true;
    state.preferences.base_url = String(args.baseUrl);
    emit();
    return;
  }
  if (name === "disconnect") {
    state = { ...state, connected: false, online: false, snapshot: null };
    emit();
    return;
  }
  if (name === "show_main") return;
  if (name === "check_updates") return "预览模式 · 当前版本 0.1.2";
  if (name === "api_request") {
    const path = String(args.path),
      body = args.body as {
        changes: Record<string, unknown>;
        expected_revision: string;
        schedulable: boolean;
        detach_managed: boolean;
      } | null;
    if (path === "/config") return structuredClone(config);
    if (path.startsWith("/config/")) {
      const section = path.split("/")[2];
      if (body?.expected_revision !== config[section].revision)
        throw new Error("设置已变化");
      config[section] = {
        ...config[section],
        ...body.changes,
        revision: Date.now().toString().padStart(64, "0"),
      };
      return structuredClone(config[section]);
    }
    if (path === "/errors" || path.startsWith("/errors?"))
      return { items: state.snapshot?.errors, next_cursor: null };
    if (path.startsWith("/errors/")) {
      const e = state.snapshot?.errors.find(
        (e) => e.id === Number(path.split("/")[2]),
      );
      return {
        ...e,
        content: JSON.stringify(
          { error: { code: e?.provider_error_code, message: e?.message } },
          null,
          2,
        ),
        content_limited: true,
      };
    }
    if (path.endsWith("/usage-action")) return { message: "预览：操作已完成" };
    if (path.startsWith("/accounts/")) {
      const a = state.snapshot?.accounts.find(
        (a) => a.id === Number(path.split("/")[2]),
      );
      if (a && body) {
        a.schedulable = body.schedulable;
        if (body.detach_managed) a.managed = false;
        emit();
      }
      return { verified: true };
    }
    if (path.startsWith("/actions/"))
      return { message: "预览：测试请求已完成", pairing_code: "DEMO-NEW1" };
  }
  throw new Error("预览未提供此操作");
}
