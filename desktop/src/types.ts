export type DegradationMark = { marked?: boolean; marked_at?: string | null; version?: string; error?: string };
export type AccountKeepalive = {
  account_id: number; enabled: boolean; generation: number; version: string;
  status: string; reason: string; paused_reason: string;
  last_restore_at: string | null; restore_count: number; budget_used: number;
  next_allowed_at: string | null; read_only: boolean;
};
export type Account = {
  degradation_mark?: DegradationMark;
  recovery_selectable?: boolean;
  model_detection?: { schedule_mode?: "fixed" | "smart"; min_interval_minutes?: number; effective_interval_minutes?: number | null; recent_request_count?: number | null; last_automatic_started_at?: string | null; enabled: boolean; interval_minutes: number; model_id: string; version: string; next_at?: string | null; status: string; reason?: string; last_result?: { status?: string; report?: { prediction_name?: string }; completed_at?: string } | null };
  id: number;
  parent_account_id?: number | null;
  name: string;
  priority: number;
  concurrency?: number | null;
  load_factor?: number | null;
  settings_capabilities?: Record<string, { editable: boolean; reason: string }>;
  platform: string;
  type: string;
  status: string;
  schedulable: boolean;
  available: boolean;
  group_ids: number[];
  blockers: { code: string; label: string; until?: string }[];
  recoverable?: boolean;
  version: string;
  operation_versions?: Record<string, string>;
  last_success_at: string | null;
  last_called_at?: string | null;
  last_error_at: string | null;
  last_error_id: number | null;
  last_error_code: string | null;
  last_error_status: number | null;
  error_message: string;
  success_after_error: boolean;
  usage_windows: UsageWindow[];
  usage?: UsageSummary;
  recovery_monitor?: { status: string; label: string; reason: string; next_at?: string | null } | null;
  auto_reset_credit?: {
    trigger_at?: string | null;
    consume_deadline_at?: string | null;
    verification_deadline_at?: string | null;
    completed_at?: string | null;
    outcome?: string | null;
    notification_status?: string | null;
    notification_attempts?: number;
    evidence_source?: "upstream_error" | "account_rate_limit";
    evidence_at?: string | null;
    stage: string;
    label: string;
    error: string;
    next_at?: string | null;
    attempt_at?: string | null;
    test_completed_at?: string | null;
    recovered_at?: string | null;
  } | null;
};
export type UsageAction =
  | "query_usage"
  | "query_reset_credits"
  | "reset_quota"
  | "probe_quota";
export type UsageStats = {
  requests: number;
  tokens: number;
  cost: number;
  standard_cost: number;
  user_cost: number;
};
export type UsageSummary = {
  branch: "none" | "apikey" | "openai_oauth" | "grok_free" | "grok_paid" | "native";
  native_read?: boolean;
  status?: "known" | "unavailable" | "error" | "authorization_required";
  message?: string;
  balances?: { currency: string; balance: number; observed_at?: string | null; status?: UsageWindow["status"] }[];
  windows: UsageWindow[];
  today: UsageStats | null;
  actions: UsageAction[];
  reset_credits: {
    available: number | null;
    expires_at: string[];
    observed_at: string | null;
  } | null;
  prepaid_balance?: number | null;
  monthly_limit?: number | null;
  monthly_used?: number | null;
};
export type UsageWindow = {
  estimated?: boolean;
  key: string;
  label: string;
  used_percent: number | null;
  reset_at: string | null;
  observed_at: string | null;
  status: "known" | "unknown" | "stale" | "error";
  source: string;
  used?: number | null;
  limit?: number | null;
  remaining?: number | null;
  color?: "indigo" | "emerald" | "purple";
  stats?: UsageStats | null;
  estimated_total_cost?: number | null;
};
export type RecentAccount = {
  log_id: number;
  account_id: number;
  account_name: string;
  model: string;
  upstream_model: string;
  called_at: string | null;
};
export type Group = {
  id: number;
  sort_order?: number;
  name: string;
  platform: string;
  account_id: number | null;
  account_name: string;
  model: string;
  upstream_model: string;
  upstream_response_model: string;
  called_at: string | null;
  recent_accounts?: RecentAccount[];
};
function callTime(value: string | null): number {
  const parsed = value ? Date.parse(value) : NaN;
  return Number.isFinite(parsed) ? parsed : Number.NEGATIVE_INFINITY;
}

export function currentGroups(groups: Group[], accounts: Account[]): Group[] {
  const members = new Map(accounts.map((account) => [account.id, account]));
  return groups.map((group) => {
    const calls = group.recent_accounts ?? (group.account_id ? [{
      log_id: 0,
      account_id: group.account_id,
      account_name: group.account_name,
      model: group.model,
      upstream_model: group.upstream_model,
      called_at: group.called_at,
    }] : []);
    const ordered = calls.filter((call) =>
      members.get(call.account_id)?.group_ids.includes(group.id),
    ).sort((a, b) => callTime(b.called_at) - callTime(a.called_at) || b.log_id - a.log_id);
    const distinct = new Map<number, RecentAccount>();
    for (const call of ordered) {
      if (!distinct.has(call.account_id)) distinct.set(call.account_id, call);
    }
    const recent = [...distinct.values()].slice(0, 3);
    return { ...group, recent_accounts: recent, called_at: recent[0]?.called_at ?? null };
  }).sort((a, b) => callTime(b.called_at) - callTime(a.called_at) || a.id - b.id);
}

export type OpsError = {
  id: number;
  account_id: number | null;
  account_name: string;
  group_id: number | null;
  group_name: string;
  created_at: string;
  model: string;
  requested_model: string;
  upstream_model: string;
  status_code: number;
  upstream_status_code: number | null;
  provider_error_code: string;
  message: string;
  request_id: string;
  resolved: boolean;
  error_owner?: string;
  error_phase?: string;
  error_source?: string;
  disposition_notification?: { status: string };
  content?: string;
  content_limited?: boolean;
  notification?: { status: string; reason?: string; at?: string; attempts?: number; next_at?: string | null };
};
export type Recovery = {
  id: number;
  account_id: number;
  account_name: string;
  model_id: string;
  test_completed_at: string | null;
  recovered_at: string | null;
  legacy: boolean;
  kind?: "quota_recovery" | "reset_credit" | "connection_test";
  result_status?: string;
  reset_credit?: { consumed?: boolean; completed_at: string; verification_method: "connection" | "model" };
};
export type Snapshot = {
  observed_at: string;
  accounts: Account[];
  groups: Group[];
  errors: OpsError[];
  recoveries: Recovery[];
};
export type Preferences = {
  base_url: string;
  favorites: number[];
  pinned: boolean;
  launch_at_login: boolean;
  record_columns?: string[] | null;
  model_test_concurrency?: number | null;
};
export type ViewState = {
  platform?: "macos" | "android";
  foreground?: boolean;
  initializing?: boolean;
  connection_revision?: number;
  connected: boolean;
  online: boolean;
  error: string;
  upstream_connection?: { state: string; endpoint: string; http_status: number | null; message: string; retryable: boolean; checked_at: string | null } | null;
  snapshot: Snapshot | null;
  preferences: Preferences;
};
export type ConfigSection = Record<string, unknown> & { revision: string };
export type Config = Record<string, ConfigSection>;
export const initialState: ViewState = {
  connected: false,
  online: false,
  error: "",
  snapshot: null,
  preferences: {
    base_url: "",
    favorites: [],
    pinned: false,
    launch_at_login: false,
  },
};
const timeFormatter = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Asia/Shanghai",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});
export function fullTime(value: string | null | undefined): string {
  if (!value) return "暂无记录";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "时间未知";
  const parts = Object.fromEntries(
    timeFormatter.formatToParts(date).map(({ type, value }) => [type, value]),
  );
  return `${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`;
}
export function sortPriority(accounts: Account[], ascending = true): Account[] {
  return [...accounts].sort(
    (a, b) => (ascending ? 1 : -1) * (a.priority - b.priority) || a.id - b.id,
  );
}
export type TestEvent = {
  type: string;
  text?: string;
  model?: string;
  error?: string;
  success?: boolean;
  image_url?: string;
  audio_url?: string;
  video_url?: string;
  duration_ms?: number;
  completed_at?: string;
};
export type QuotaBatch = {
  id: string;
  status: string;
  total: number;
  completed: number;
  started_at: string;
  completed_at: string | null;
  items: {
    account_id: number;
    account_name: string;
    platform: string;
    status: string;
    error?: string;
    usage?: UsageSummary;
  }[];
};
export function filterAccounts(
  accounts: Account[],
  query: string,
  group: string,
  platform: string,
  status: string,
  type = "",
): Account[] {
  return accounts.filter(
    (a) =>
      (!query ||
        `${a.id} ${a.name}`.toLowerCase().includes(query.toLowerCase())) &&
      (!group || a.group_ids.includes(Number(group))) &&
      (!platform || a.platform === platform) &&
      (!type || a.type === type) &&
      (!status ||
        (status === "ready"
          ? a.available
          : status === "error"
              ? !!a.last_error_id
              : !a.schedulable)),
  );
}

export function sortRecentCall(accounts: Account[], ascending = false): Account[] {
  const timestamp = (account: Account) => {
    const recent = callTime(account.last_called_at ?? null);
    return Number.isFinite(recent) ? recent : callTime(account.last_success_at);
  };
  return [...accounts].sort((a, b) => {
    const left = timestamp(a), right = timestamp(b);
    if (!Number.isFinite(left)) return !Number.isFinite(right) ? b.id - a.id : 1;
    if (!Number.isFinite(right)) return -1;
    return (ascending ? 1 : -1) * (left - right) || b.id - a.id;
  });
}
