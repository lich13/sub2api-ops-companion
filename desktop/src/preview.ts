// Development-only, deterministic fixtures. The native build never uses this transport.
import type { ViewState, Account, Config, TestEvent, Preferences } from "./types";
import { version as appVersion } from "../package.json";
const now = new Date().toISOString(),
  before = new Date(Date.now() - 7 * 60e3).toISOString();
const accounts: Account[] = [
  {
    id: 101,
    priority: 1,
    name: "Codex · 主力",
    platform: "openai",
    type: "oauth",
    status: "active",
    schedulable: true,
    available: true,
    group_ids: [1],
    blockers: [],
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
    priority: 5,
    name: "Codex · 备用账号 · 用于验证长名称的显示与调度开关",
    platform: "openai",
    type: "oauth",
    status: "error",
    recoverable: true,
    schedulable: true,
    available: false,
    group_ids: [1],
    blockers: [{ code: "reauth", label: "需要重新认证" }],
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
    priority: 2,
    name: "Grok · Super",
    platform: "grok",
    type: "oauth",
    status: "active",
    schedulable: true,
    available: false,
    group_ids: [2],
    blockers: [{ code: "rate_limit_reset_at", label: "限流中", until: new Date(Date.now() + (11 * 24 + 19) * 3600_000).toISOString() }],
    version: "c".repeat(64),
    last_success_at: before,
    last_error_at: now,
    last_error_id: 9002,
    last_error_code: "rate_limit_exceeded",
    last_error_status: 429,
    error_message: "Too many requests.",
    success_after_error: false,
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
    priority: 50,
    name: "OpenAI · 按量备用",
    platform: "openai",
    type: "apikey",
    status: "active",
    schedulable: false,
    available: false,
    group_ids: [1],
    blockers: [{ code: "disabled", label: "调度已关闭" }],
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
  account.concurrency = 2; account.load_factor = null;
  account.operation_versions = Object.fromEntries(['priority', 'concurrency', 'load_factor', 'model_config', 'proxy', 'sync_models', 'groups', 'recover', 'usage', 'reset_quota', 'delete', 'test', 'schedulable', 'model_test'].map((key) => [key, account.version]));
  if (account.platform === 'openai' && ['oauth', 'apikey'].includes(account.type)) account.degradation_mark = { marked: account.id === 102, version: 'f'.repeat(64) };

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
  group_ids: [2],
  platform: "grok",
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
  parent_account_id: 201,
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
if (new URLSearchParams(location.search).get("scenario") === "native-usage") {
  accounts.push({ ...accounts[3], id: 392, name: "Claude · Key", platform: "anthropic", type: "apikey",
    usage: { branch: "apikey", windows: [], today: stats, actions: [], reset_credits: null } });
  accounts.push({ ...accounts[3], id: 393, name: "Gemini · 配额", platform: "gemini", type: "service_account",
    usage: { branch: "native", native_read: true, status: "known", windows: [{ key: "gemini_shared_daily", label: "共享 1d", used_percent: 0, observed_at: now, reset_at: new Date(Date.now()+3600000).toISOString(), status: "known", source: "local_estimate", estimated: true }], today: stats, actions: ["query_usage"], reset_credits: null } });
  accounts.push({ ...accounts[3], id: 394, name: "DeepSeek · 余额", platform: "deepseek", type: "apikey",
    usage: { branch: "apikey", native_read: true, status: "known", windows: [], today: stats, balances: [{ currency: "USD", balance: 0, status: "known", observed_at: now }, { currency: "CNY", balance: 42, status: "known", observed_at: now }], actions: ["query_usage"], reset_credits: null } });
}
for (const account of accounts.filter(a => a.type === "apikey")) {
  if (!account.usage) continue;
  account.usage.native_read = true;
  if (!account.usage.actions.includes("query_usage")) account.usage.actions.unshift("query_usage");
  account.usage.key_quota = {
    state: "ok", source: "sub2api", checked_at: Date.now()/1000, success_at: Date.now()/1000,
    retry_at: null, next_refresh_at: null, stale: false, error: null, key_status: null,
    plans: [{name:"钱包余额",remaining:12.5,used:null,total:null,unit:"USD",unlimited:false,reset_at:null}],
    subscriptions: [{name:"标准套餐",status:"active",active:true,expires_at:new Date(Date.now()+86400000*30).toISOString(),plans:[
      {name:"每日",remaining:15,used:5,total:20,unit:"USD",unlimited:false,reset_at:new Date(Date.now()+3600000).toISOString()},
      {name:"每周",remaining:80,used:20,total:100,unit:"USD",unlimited:false,reset_at:null},
    ]}], expires_at:null, expires_at_unix:null,
    today:{requests:25,tokens:16000,cost:1.2},total_usage:{requests:180,tokens:128000,cost:8.4},
  };
}
let state: ViewState = {
  platform: new URLSearchParams(location.search).has("mobile") ? "android" : "macos",
  foreground: true,
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
        recent_accounts: [2, 4, 5].map((index, rank) => ({
          log_id: 90 - rank,
          account_id: accounts[index].id,
          account_name: accounts[index].name,
          model: "grok-4.6",
          upstream_model: "grok-4.6",
          called_at: rank ? before : now,
        })),
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
    recoveries: [
      {
        id: 2,
        account_id: 101,
        account_name: accounts[0].name,
        model_id: "gpt-6-luna",
        test_completed_at: before,
        recovered_at: now,
        legacy: false,
        kind: "reset_credit",
        attempt_count: 3,
        reset_credit: { consumed: true, completed_at: before, verification_method: "model" },
      },
      {
        id: 1,
        account_id: 102,
        account_name: accounts[1].name,
        model_id: "gpt-5.6-luna",
        test_completed_at: before,
        recovered_at: now,
        legacy: false,
      },
      {
        id: 3,
        account_id: 101,
        account_name: accounts[0].name,
        model_id: "gpt-6-sol",
        test_completed_at: before,
        recovered_at: null,
        legacy: false,
        kind: "connection_test",
        attempt_count: 1,
        result_status: "completed",
      },
    ],
  },
};
const config: Config = {
  oauth: {
    revision: "1".repeat(64),
    oauth_recovery_monitor_enabled: true,
    oauth_early_reset_enabled: false,
    oauth_early_reset_interval_seconds: 60,
    oauth_auto_reset_credit_enabled: false,
    oauth_recovery_connection_account_ids: accounts.filter(a => a.platform === "openai" && a.type === "oauth").map(a => a.id),
    oauth_recovery_model_account_ids: [],
    oauth_daily_test_enabled: true,
    oauth_daily_test_time: "05:00",
    oauth_usage_refresh_concurrency: 4,
    oauth_recovery_test_concurrency: 2,
    oauth_early_probe_batch_size: 8,
    connection_test_retry_interval_seconds: 10,
    connection_test_max_retries: 3,
    oauth_recovery_test_model_id: "gpt-5.6-luna",
    model_detection_model_id: "gpt-6-luna",
    model_detection_platforms: { openai: { engine: "modeltrace", sample_count: 1 } },
  },
  bark: { revision: "2".repeat(64), enabled: true, device_key_set: true },
};
const listeners = new Set<(s: ViewState) => void>();
const scenario = new URLSearchParams(location.search).get("scenario");
if (state.snapshot && scenario?.startsWith("lm-")) {
  accounts[0].name = "模型检测与分词器验证 · 跨区域共享账号 · 长名称排版预览";
  state.preferences.model_test_preferences = { [`${state.preferences.base_url}:101`]: { engine: "lm_detector", sample_count: 3 } };
} else if (state.snapshot && scenario === "priority-platforms") {
  state.snapshot.accounts.push({ ...structuredClone(accounts[0]), id: 601, name: "Claude · 长名称预览账号", platform: "anthropic", priority: 7, group_ids: [] });
} else if (state.snapshot && scenario === "group-manager") {
  const empty = state.snapshot.groups[2];
  state.snapshot.groups = [
    { ...empty, id: 1, name: "codex羊毛", sort_order: 0 },
    { ...empty, id: 3, name: "codex爽用", sort_order: 1 },
    { ...empty, id: 2, name: "grok爽用", platform: "grok", sort_order: 0 },
    { ...empty, id: 4, name: "grok羊毛", platform: "grok", sort_order: 1 },
  ];
  accounts[0].group_ids = [1]; accounts[1].group_ids = [1, 3];
  accounts.push({ ...accounts[0], id: 104, name: "研发共享账号 · 長名称-跨区域验证-long-name@example.com", group_ids: [3], version: "d".repeat(64) });
  accounts.push({ ...accounts[0], id: 105, name: "Codex 待分组", group_ids: [], version: "e".repeat(64) });
  accounts[2].group_ids = [2, 4];
  for (const account of accounts) if (account.platform === "openai" && account.type === "oauth") account.degradation_mark = { marked: account.id === 102, marked_at: account.id === 102 ? before : null, version: "f".repeat(64) };
} else if (state.snapshot && scenario === "group-membership") {
  const [codex, grok, empty] = state.snapshot.groups;
  accounts[0].group_ids = [1, 4];
  accounts[1].group_ids = [1, 2];
  const outside = { ...grok.recent_accounts![0], log_id: 999 };
  codex.called_at = now;
  codex.recent_accounts = [outside, ...codex.recent_accounts!.map((call) => ({ ...call, called_at: before }))];
  grok.recent_accounts!.push({ ...codex.recent_accounts[2], log_id: 80 });
  state.snapshot.groups = [
    { ...empty, id: 5, name: "从未调用" },
    { ...codex, id: 4, name: "同时调用", recent_accounts: [codex.recent_accounts[1]] },
    { ...empty, account_id: outside.account_id, account_name: outside.account_name, called_at: now, recent_accounts: [outside] },
    codex,
    grok,
  ];
} else if (state.snapshot && scenario === "record-filters") {
  accounts[0].name = "研发团队共享账户 · 跨区域项目与模型验证专用 · 长名称换行验收";
} else if (state.snapshot && scenario === "auto-reset") {
  accounts[0].schedulable = false;
  accounts[0].available = false;
  accounts[0].auto_reset_credit = {
    stage: "retry", label: "等待重试测活", error: "测活失败",
    attempt_at: before, test_completed_at: now,
    next_at: new Date(Date.now() + 60e3).toISOString(),
  };
} else if (state.snapshot && scenario === "empty") {
  Object.assign(state.snapshot, {
    accounts: [],
    groups: [],
    errors: [],
    recoveries: [],
  });
} else if (state.snapshot && scenario === "dense") {
  state.snapshot.groups = Array.from({ length: 6 }, (_, i) => ({
    ...state.snapshot!.groups[i % 2],
    id: i + 10,
    name: `分组 ${i + 1}`,
  }));
  for (const account of accounts) {
    account.group_ids = state.snapshot.groups.filter((group) =>
      group.recent_accounts?.some((call) => call.account_id === account.id),
    ).map((group) => group.id);
  }
} else if (state.snapshot && scenario === "two-groups") {
  state.snapshot.groups = state.snapshot.groups.slice(0, 2);
}
function emit() {
  for (const cb of listeners) cb(structuredClone(state));
}
let previewQuotaSeconds = 60;
const previewQuotaTokens = new Map<number, string>();
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
    if (args.recordColumns) state.preferences.record_columns = args.recordColumns as string[];
    if (args.modelTestPreferences) state.preferences.model_test_preferences = structuredClone(args.modelTestPreferences) as Preferences["model_test_preferences"];
    if (args.modelTestConcurrency) state.preferences.model_test_concurrency = Number(args.modelTestConcurrency);
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
  if (["show_main", "resize_quick", "show_quick", "hide_quick"].includes(name))
    return;
  if (name === "open_authorization_url") return;
  if (name === "check_updates") return `预览模式 · 当前版本 ${appVersion}`;
  if (name === "cancel_test") {
    testCancelled = true;
    return;
  }
  if (name === "api_request") {
    if (String(args.path).startsWith("/sub2api-config")) return (await import("./sub2apiConfigPreview")).configPreview(String(args.path), (args.body || {}) as Record<string,unknown>);
    if (args.path === "/config/key-quota") {
      if (args.method === "PUT") previewQuotaSeconds = Number((args.body as {seconds:number}).seconds);
      return {seconds:previewQuotaSeconds,revision:"a".repeat(64)};
    }
    if (/^\/accounts\/\d+\/key-quota-settings$/.test(String(args.path))) {
      const id = Number(String(args.path).split("/")[2]);
      if (args.method === "PUT" && Object.prototype.hasOwnProperty.call((args.body || {}) as object, "access_token")) previewQuotaTokens.set(id, (args.body as {access_token:string}).access_token);
      return {account_id:id,has_access_token:!!previewQuotaTokens.get(id),revision:"b".repeat(64)};
    }

    if (/^\/accounts\/\d+\/key-quota-settings\/access-token$/.test(String(args.path))) {
      const id=Number(String(args.path).split("/")[2]), token=previewQuotaTokens.get(id);
      if(!token) throw new Error("尚未配置账户访问令牌 [404]");
      return {account_id:id,access_token:token};
    }
    if (/^\/accounts\/\d+\/apply-oauth-credentials$/.test(String(args.path))) return { account_id: Number(String(args.path).split("/")[2]), verified: true, status: "active" };
    if (String(args.path).startsWith("/account-creation")) return (await import("./accountCreationPreview")).creationPreview(String(args.path),(args.body||{}) as Record<string,unknown>,state.snapshot?.accounts??[],emit);
    if (args.path === "/capabilities") return { api_version: 1, features: { account_creation: true, key_quota_token_reveal: true, detection_logs: true, global_detection_model: true, account_priority_ordering: true, account_priority_per_platform: true } };
    if (String(args.path).split("?")[0] === "/account-priorities" || String(args.path).startsWith("/account-priority-orders")) return (await import("./priorityPreview")).priorityPreview(String(args.method), String(args.path), (args.body || {}) as Record<string, unknown>, state.snapshot?.accounts ?? [], emit);
    if (String(args.path).startsWith("/detection-logs")) return (await import("./detectionLogPreview")).detectionLogPreview(String(args.path));
    if (args.method === "POST" && typeof args.path === "string" && /^\/proxies\/[1-9]\d*\/(test|quality-check)$/.test(args.path)) {
      if (args.path.endsWith("/test")) return { success: true, message: "连接成功", latency_ms: 85, ip_address: "192.0.2.1", city: "", region: "", country: "示例地区", country_code: "" };
      return { score: 90, grade: "A", summary: "原生质量检测通过", exit_ip: "192.0.2.1", country: "示例地区", country_code: "", base_latency_ms: 85, passed_count: 1, warn_count: 0, failed_count: 0, challenge_count: 0, checked_at: Math.floor(Date.now() / 1000), items: [{ target: "OpenAI", status: "pass", http_status: 200, latency_ms: 90, message: "通过" }] };
    }
    if (String(args.path).startsWith("/proxy-management")) return (await import("./proxyPreview")).proxyPreview(String(args.method),String(args.path),(args.body || {}) as Record<string,unknown>);
    if (args.path === "/proxies") return structuredClone((await import('./settingsPreview')).previewProxies);
    if (/^\/accounts\/\d+\/settings$/.test(String(args.path))) {
      const account = state.snapshot?.accounts.find(a => a.id === Number(String(args.path).split('/')[2]));
      if (!account) throw new Error('账号不存在');
      return (await import('./settingsPreview')).settingsPreview(account);
    }
    if (args.path === "/connection-status") return { state: "ok", endpoint: "verify", http_status: 200, message: "Sub2API 管理权限已验证", retryable: false, checked_at: new Date().toISOString() };
    if (/^\/accounts\/[1-9]\d*\/keepalive$/.test(String(args.path))) return (await import('./keepalivePreview')).keepalivePreview(String(args.method), String(args.path), (args.body || {}) as Record<string, unknown>, state.snapshot?.accounts ?? []);
    if (String(args.path).startsWith('/account-templates') || String(args.path).startsWith('/modeltrace/fingerprint-bank') || String(args.path).startsWith('/lm-detector/fingerprint-bank') || /^\/accounts\/\d+\/model-detection$/.test(String(args.path))) return (await import('./detectionPreview')).detectionPreview(String(args.method), String(args.path), (args.body || {}) as Record<string, unknown>, state.snapshot?.accounts ?? [], String(config.oauth.model_detection_model_id), config.oauth.model_detection_platforms);
    if (String(args.path).startsWith('/account-operations') || /^\/accounts\/\d+\/operations$/.test(String(args.path))) return (await import('./operationPreview')).operationPreview(String(args.method), String(args.path), (args.body || {}) as Record<string, unknown>, state.snapshot?.accounts ?? [], emit);
    if (/^\/accounts\/\d+\/model-tests(?:\/latest)?$/.test(String(args.path)) || String(args.path).startsWith("/model-tests/")) {
      const result = (await import("./modelTestPreview")).modelTestPreview(String(args.method), String(args.path), (args.body || {}) as Record<string, unknown>, state.snapshot?.accounts ?? []);
      if (scenario === "lm-offline" && String(args.path).endsWith("/latest")) {
        setTimeout(() => { state.online = false; emit(); }, 100);
      }
      return result;
    }
    if (String(args.method) === "PUT" && /^\/accounts\/\d+\/(groups|degradation-mark)$/.test(String(args.path))) {
      const path = String(args.path), body = args.body as Record<string, unknown>;
      const account = state.snapshot?.accounts.find((a) => a.id === Number(path.split("/")[2]));
      if (!account) throw new Error("账号不存在");
      if (path.endsWith("/groups")) {
        if (body.expected_version !== account.version) throw new Error("账号已变化，请刷新");
        const scope = body.scope_group_ids as number[], selected = body.group_ids as number[];
        account.group_ids = [...new Set([...account.group_ids.filter((id) => !scope.includes(id)), ...selected])].sort((a, b) => a - b);
        account.version = Date.now().toString(16).padEnd(64, "0"); emit();
        return { verified: true, group_ids: account.group_ids, version: account.version };
      }
      if (body.expected_mark_version !== account.degradation_mark?.version) throw new Error("降智标记已变化，请刷新");
      account.degradation_mark = { marked: !!body.marked, marked_at: body.marked ? new Date().toISOString() : null, version: Date.now().toString(16).padEnd(64, "0") }; emit();
      return { verified: true, degradation_mark: account.degradation_mark };
    }
    if (String(args.path).startsWith("/usage-records") || String(args.path).startsWith("/usage-record-options")) {
      return (await import("./recordPreview")).recordPreview(String(args.path), accounts);
    }
    if (String(args.path).startsWith("/model-")) {
      return (await import("./modelPreview")).modelPreview(String(args.method), String(args.path), (args.body || {}) as Record<string, unknown>);
    }
    const path = String(args.path),
      body = args.body as {
        changes: Record<string, unknown>;
        expected_revision: string;
        schedulable: boolean;
        priority: number;
      } | null;

    if (args.method === "DELETE" && /^\/accounts\/\d+$/.test(path)) {
      const id = Number(path.split("/")[2]);
      const account = state.snapshot?.accounts.find((a) => a.id === id);
      if (!account || !state.snapshot) throw new Error("账号不存在");
      state.snapshot.accounts = state.snapshot.accounts.filter(
        (a) => a.id !== id,
      );
      for (const group of state.snapshot.groups)
        group.recent_accounts = group.recent_accounts?.filter(
          (a) => a.account_id !== id,
        );
      emit();
      return { deleted: true, verified: true };
    }
    if (path.endsWith("/recover-state")) {
      const account = state.snapshot?.accounts.find(
        (a) => a.id === Number(path.split("/")[2]),
      );
      if (!account) throw new Error("账号不存在");
      Object.assign(account, {
        status: "active",
        recoverable: false,
        blockers: [],
        available: account.schedulable,
      });
      emit();
      return { verified: true };
    }
    if (path === "/config") return structuredClone(config);
    if (path.startsWith("/config/")) {
      if (scenario === "config-save-failure" && args.method === "PUT") throw new Error("设置保存失败，请重试");
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
    if (path === "/errors" || path.startsWith("/errors?")) {
      const params = new URLSearchParams(path.split("?")[1]);
      const category = params.get("category");
      const before = Number(params.get("before_id")) || Infinity;
      const items = (state.snapshot?.errors ?? []).filter(e => {
        const account = state.snapshot?.accounts.find(a => a.id === e.account_id);
        const degradation = account?.platform === "openai" && ["oauth", "apikey"].includes(account.type)
          && ["Our servers are currently overloaded. Please try again later.", "Selected model is at capacity. Please try a different model.", "stream disconnected before completion: Concurrency limit exceeded for account, please retry later"].includes(e.message);
        return e.id < before && (!category || (category === "degradation" ? degradation : !degradation));
      });
      return { items, next_cursor: null };
    }
    if (path.startsWith("/recoveries"))
      return { items: [...(await import("./operationPreview")).scheduledTestHistory(), ...(state.snapshot?.recoveries ?? [])], next_cursor: null };
    if (path === "/quota-refresh")
      return args.method === "POST"
        ? {
            id: "preview",
            status: "completed",
            total: 3,
            completed: 3,
            items: [],
            started_at: now,
            completed_at: now,
          }
        : { status: "idle", total: 0, completed: 0, items: [] };
    if (path.split("?", 1)[0].endsWith("/models")) {
      const query = new URLSearchParams(path.split("?")[1]);
      if (scenario === "model-directory-error" && query.get("refresh") === "true") throw new Error("模型目录暂不可用，请重试");
      if (query.get("purpose") === "model_test") {
        const account = state.snapshot?.accounts.find((item) => item.id === Number(path.split("/")[2]));
        if (!account) throw new Error("账号不存在");
        return (await import("./modelTestPreview")).modelTestCandidates(account);
      }
      return [
        "gpt-6-sol",
        "gpt-image-1",
        "grok-4.5",
        "grok-imagine-image",
        "grok-imagine-video",
      ].map((id) => ({ id, display_name: id, type: "model" }));
    }
    if (path.startsWith("/errors/")) {
      const e = state.snapshot?.errors.find(
        (e) => e.id === Number(path.split("?")[0].split("/")[2]),
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
    if (/^\/accounts\/\d+\/usage$/.test(path)) return accounts.find(a => a.id === Number(path.split("/")[2]))?.usage;
    if (path.endsWith("/usage-action")) return { message: "预览：操作已完成" };
    if (path.startsWith("/accounts/")) {
      const a = state.snapshot?.accounts.find(
        (a) => a.id === Number(path.split("/")[2]),
      );
      if (a && body) {
        if (path.endsWith("/priority")) a.priority = body.priority;
        else a.schedulable = body.schedulable;
        emit();
      }
      return { verified: true };
    }
    if (path.startsWith("/actions/"))
      return { message: "预览：测试请求已完成", pairing_code: "DEMO-NEW1" };
  }
  throw new Error("预览未提供此操作");
}
let testCancelled = false;
export async function testStream(
  body: Record<string, unknown>,
  callback: (event: TestEvent) => void,
) {
  testCancelled = false;
  callback({ type: "test_start", model: String(body.model_id || body.mode) });
  for (const text of [
    "Hello",
    " ",
    "world!\n",
    "  Preview output\n",
    "\t中文 👋\n",
  ]) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    if (testCancelled) return;
    callback({ type: "content", text });
  }
  callback({
    type: "test_complete",
    success: true,
    completed_at: new Date().toISOString(),
    duration_ms: 1000,
  });
}
