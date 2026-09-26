export type Account = {
  id: number;
  name: string;
  priority: number;
  platform: string;
  type: string;
  status: string;
  schedulable: boolean;
  available: boolean;
  group_ids: number[];
  blockers: { code: string; label: string; until?: string }[];
  managed: boolean;
  recoverable?: boolean;
  version: string;
  last_success_at: string | null;
  last_error_at: string | null;
  last_error_id: number | null;
  last_error_code: string | null;
  last_error_status: number | null;
  error_message: string;
  success_after_error: boolean;
  usage_windows: UsageWindow[];
  usage?: UsageSummary;
  quality?: Quality;
};
export type Quality = {
  score: number | null;
  grade: "red" | "yellow" | "green";
  reasons: string[];
  sample_status: "complete" | "insufficient" | "empty" | "pending";
  data_status: "fresh" | "delayed" | "stale";
  computed_at: string | null;
};
export type QualityCohort = {
  platform: string;
  model: string;
  reasoning_effort: string;
  service_tier: string;
  transport: string;
  input_bucket: string;
  samples: number;
  p50: number;
  tail: number;
  baseline_p50: number;
  baseline_tail: number;
  baseline_accounts: number;
  baseline_samples: number;
  score: number;
};
export type QualityMetric = {
  score: number | null;
  samples: number;
  compared: number;
  coverage: number;
  p50: number | null;
  tail: number | null;
  mode: string;
  cohorts: QualityCohort[];
  recent: QualityMetric;
  history: QualityMetric;
  all: QualityMetric;
};
export type QualityDetail = Quality & {
  account_id: number;
  cap?: number;
  coverage?: number;
  consecutive_failures?: number;
  period?: { start: string; end: string; recent_start: string };
  reliability?: {
    score: number | null;
    rate: number | null;
    effective_rate: number | null;
    successes: number;
    failures: number;
    total: number;
    mode: string;
    causes: Record<string, number>;
  };
  ttft?: QualityMetric;
  throughput?: QualityMetric;
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
  branch: "none" | "apikey" | "openai_oauth" | "grok_free" | "grok_paid";
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
  called_at: string;
};
export type Group = {
  id: number;
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
export type OpsError = {
  id: number;
  account_id: number;
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
  content?: string;
  content_limited?: boolean;
};
export type Recovery = {
  id: number;
  account_id: number;
  account_name: string;
  model_id: string;
  test_completed_at: string | null;
  recovered_at: string | null;
  legacy: boolean;
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
};
export type ViewState = {
  connection_revision?: number;
  connected: boolean;
  online: boolean;
  error: string;
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
export function sortQuality(accounts: Account[], ascending = true): Account[] {
  return [...accounts].sort((a, b) => {
    const left = a.quality?.score,
      right = b.quality?.score;
    if (left == null) return right == null ? a.id - b.id : 1;
    if (right == null) return -1;
    return (ascending ? 1 : -1) * (left - right) || a.id - b.id;
  });
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
          : status === "managed"
            ? a.managed
            : status === "error"
              ? !!a.last_error_id
              : !a.schedulable)),
  );
}
