import type {
  AccountBlockerRecord,
  AccountRecord,
  AutoResetCreditStateRecord,
  ConnectionReply,
  DegradationMarkRecord,
  GroupRecord,
  KeepaliveReply,
  KeyQuotaRecord,
  KeyQuotaUsageRecord,
  ModelDetectionSummaryRecord,
  OpsErrorRecord,
  QuotaPlanRecord,
  QuotaSubscriptionRecord,
  RecentAccountRecord,
  RecoveryMonitorStateRecord,
  RecoveryRecord,
  RecoveryResetCreditRecord,
  ResetCreditsInfoRecord,
  SnapshotRecord,
  UsageBalanceRecord,
  UsageStatsRecord,
  UsageSummaryRecord,
  UsageWindowRecord,
} from "./protocol";
export type {
  TestEventRecord as TestEvent,
  UsageStatsRecord as UsageStats,
} from "./protocol";

type WithNull<T, K extends keyof T> = Omit<T, K> & {
  [P in K]?: Exclude<T[P], undefined> | null;
};
type WithRequired<T, K extends keyof T> = Omit<T, K> & Required<Pick<T, K>>;

export type DegradationMark = WithNull<DegradationMarkRecord, "marked_at">;
export type AccountKeepalive = Omit<
  KeepaliveReply,
  "last_restore_at" | "next_allowed_at"
> & {
  last_restore_at: string | null;
  next_allowed_at: string | null;
};
type AccountBlocker = WithRequired<AccountBlockerRecord, "code" | "label">;
type ModelDetectionSummary = Omit<
  WithNull<
    WithRequired<
      ModelDetectionSummaryRecord,
      "enabled" | "interval_minutes" | "model_id" | "version"
    >,
    | "effective_interval_minutes"
    | "recent_request_count"
    | "last_automatic_started_at"
    | "next_at"
    | "last_result"
  >,
  "schedule_mode"
> & {
  schedule_mode?: "fixed" | "smart";
};
export type Account = Omit<
  WithRequired<
    AccountRecord,
    "status" | "error_message" | "success_after_error"
  >,
  | "degradation_mark"
  | "model_detection"
  | "parent_account_id"
  | "concurrency"
  | "load_factor"
  | "blockers"
  | "last_success_at"
  | "last_called_at"
  | "last_error_at"
  | "last_error_id"
  | "last_error_code"
  | "last_error_status"
  | "usage_windows"
  | "usage"
  | "recovery_monitor"
  | "auto_reset_credit"
> & {
  degradation_mark?: DegradationMark;
  model_detection?: ModelDetectionSummary;
  parent_account_id?: number | null;
  concurrency?: number | null;
  load_factor?: number | null;
  blockers: AccountBlocker[];
  last_success_at: string | null;
  last_called_at?: string | null;
  last_error_at: string | null;
  last_error_id: number | null;
  last_error_code: string | null;
  last_error_status: number | null;
  usage_windows: UsageWindow[];
  usage?: UsageSummary;
  recovery_monitor?: WithNull<
    RecoveryMonitorStateRecord,
    "next_at" | "next_retry_at"
  > | null;
  auto_reset_credit?:
    | (Omit<
        WithNull<
          AutoResetCreditStateRecord,
          | "trigger_at"
          | "consume_deadline_at"
          | "verification_deadline_at"
          | "completed_at"
          | "outcome"
          | "notification_status"
          | "evidence_at"
          | "next_at"
          | "attempt_at"
          | "test_completed_at"
          | "recovered_at"
        >,
        "evidence_source"
      > & {
        evidence_source?: "upstream_error" | "account_rate_limit";
      })
    | null;
};
export type UsageAction =
  "query_usage" | "query_reset_credits" | "reset_quota" | "probe_quota";
export type QuotaPlan = Omit<
  QuotaPlanRecord,
  "remaining" | "used" | "total" | "reset_at"
> & {
  remaining: number | null;
  used: number | null;
  total: number | null;
  reset_at: string | null;
};
export type KeyQuota = Omit<
  KeyQuotaRecord,
  | "state"
  | "source"
  | "checked_at"
  | "success_at"
  | "retry_at"
  | "next_refresh_at"
  | "error"
  | "key_status"
  | "plans"
  | "subscriptions"
  | "expires_at"
  | "expires_at_unix"
  | "today"
  | "total_usage"
> & {
  state: "idle" | "loading" | "ok" | "error" | "unsupported";
  source: "sub2api" | "newapi" | null;
  checked_at: number | null;
  success_at: number | null;
  retry_at: number | null;
  next_refresh_at: number | null;
  error: string | null;
  key_status: string | null;
  plans: QuotaPlan[];
  subscriptions: (Omit<
    QuotaSubscriptionRecord,
    "name" | "status" | "active" | "expires_at" | "plans"
  > & {
    name: string | null;
    status: string | null;
    active: boolean | null;
    expires_at: string | null;
    plans: QuotaPlan[];
  })[];
  expires_at: string | null;
  expires_at_unix: number | null;
  today:
    | {
        [K in keyof KeyQuotaUsageRecord]-?: Exclude<
          KeyQuotaUsageRecord[K],
          undefined
        > | null;
      }
    | null;
  total_usage:
    | {
        [K in keyof KeyQuotaUsageRecord]-?: Exclude<
          KeyQuotaUsageRecord[K],
          undefined
        > | null;
      }
    | null;
};
export type UsageSummary = Omit<
  UsageSummaryRecord,
  | "stats_status"
  | "key_quota"
  | "branch"
  | "status"
  | "balances"
  | "windows"
  | "today"
  | "actions"
  | "reset_credits"
  | "prepaid_balance"
  | "monthly_limit"
  | "monthly_used"
> & {
  stats_status?: "ok" | "error";
  key_quota?: KeyQuota;
  branch:
    "none" | "apikey" | "openai_oauth" | "grok_free" | "grok_paid" | "native";
  status?: "known" | "unavailable" | "error" | "authorization_required";
  balances?: (Omit<UsageBalanceRecord, "observed_at" | "status"> & {
    observed_at?: string | null;
    status?: UsageWindow["status"];
  })[];
  windows: UsageWindow[];
  today: UsageStatsRecord | null;
  actions: UsageAction[];
  reset_credits:
    | (Omit<ResetCreditsInfoRecord, "available" | "observed_at"> & {
        available: number | null;
        observed_at: string | null;
      })
    | null;
  prepaid_balance?: number | null;
  monthly_limit?: number | null;
  monthly_used?: number | null;
};
export type UsageWindow = Omit<
  UsageWindowRecord,
  | "used_percent"
  | "reset_at"
  | "observed_at"
  | "status"
  | "used"
  | "limit"
  | "remaining"
  | "color"
  | "stats"
  | "estimated_total_cost"
> & {
  used_percent: number | null;
  reset_at: string | null;
  observed_at: string | null;
  status: "known" | "unknown" | "stale" | "error";
  used?: number | null;
  limit?: number | null;
  remaining?: number | null;
  color?: "indigo" | "emerald" | "purple";
  stats?: UsageStatsRecord | null;
  estimated_total_cost?: number | null;
};
export type RecentAccount = Omit<
  WithRequired<
    RecentAccountRecord,
    "log_id" | "account_id" | "account_name" | "model" | "upstream_model"
  >,
  "called_at"
> & {
  called_at: string | null;
};
export type Group = Omit<
  WithRequired<
    GroupRecord,
    "platform" | "account_name" | "model" | "upstream_model"
  >,
  "account_id" | "called_at" | "recent_accounts"
> & {
  account_id: number | null;
  called_at: string | null;
  recent_accounts: RecentAccount[];
};
function callTime(value: string | null): number {
  const parsed = value ? Date.parse(value) : NaN;
  return Number.isFinite(parsed) ? parsed : Number.NEGATIVE_INFINITY;
}

export function currentGroups(groups: Group[], accounts: Account[]): Group[] {
  const members = new Map(accounts.map((account) => [account.id, account]));
  return groups
    .map((group) => {
      const ordered = group.recent_accounts
        .filter((call) =>
          members.get(call.account_id)?.group_ids.includes(group.id),
        )
        .sort(
          (a, b) =>
            callTime(b.called_at) - callTime(a.called_at) ||
            b.log_id - a.log_id,
        );
      const distinct = new Map<number, RecentAccount>();
      for (const call of ordered) {
        if (!distinct.has(call.account_id)) distinct.set(call.account_id, call);
      }
      const recent = [...distinct.values()].slice(0, 3);
      return {
        ...group,
        recent_accounts: recent,
        called_at: recent[0]?.called_at ?? null,
      };
    })
    .sort(
      (a, b) => callTime(b.called_at) - callTime(a.called_at) || a.id - b.id,
    );
}

export type OpsError = Omit<
  WithRequired<
    OpsErrorRecord,
    | "id"
    | "account_name"
    | "group_name"
    | "created_at"
    | "model"
    | "requested_model"
    | "upstream_model"
    | "status_code"
    | "provider_error_code"
    | "message"
    | "request_id"
    | "resolved"
  >,
  "account_id" | "group_id" | "upstream_status_code" | "notification"
> & {
  account_id: number | null;
  group_id: number | null;
  upstream_status_code: number | null;
  notification?: WithNull<
    NonNullable<OpsErrorRecord["notification"]>,
    "next_at"
  >;
};
export type Recovery = Omit<
  WithRequired<
    RecoveryRecord,
    "id" | "account_id" | "account_name" | "model_id" | "legacy"
  >,
  "test_completed_at" | "recovered_at" | "kind" | "reset_credit"
> & {
  test_completed_at: string | null;
  recovered_at: string | null;
  kind?: "quota_recovery" | "reset_credit" | "connection_test";
  reset_credit?: Omit<RecoveryResetCreditRecord, "verification_method"> & {
    verification_method: "connection" | "model";
  };
};
type Snapshot = Omit<
  SnapshotRecord,
  "accounts" | "groups" | "errors" | "recoveries"
> & {
  accounts: Account[];
  groups: Group[];
  errors: OpsError[];
  recoveries: Recovery[];
};
export type SavedConnection = {
  name: string;
  base_url: string;
};
export type Preferences = {
  model_test_preferences?: Record<
    string,
    import("./modelTestTypes").ModelTestPreference
  >;
  base_url: string;
  connections?: SavedConnection[];
  record_columns?: string[] | null;
  model_test_concurrency?: number | null;
};
export function defaultConnectionName(baseUrl: string): string {
  const trimmed = baseUrl.trim().replace(/\/+$/, "");
  if (!trimmed) return "未命名实例";
  try {
    const parsed = new URL(trimmed);
    return parsed.host || trimmed;
  } catch {
    return trimmed;
  }
}
export function savedConnections(preferences: Preferences): SavedConnection[] {
  if (preferences.connections?.length) {
    return preferences.connections.map((item) => ({
      name: item.name || defaultConnectionName(item.base_url),
      base_url: item.base_url,
    }));
  }
  return preferences.base_url
    ? [
        {
          name: defaultConnectionName(preferences.base_url),
          base_url: preferences.base_url,
        },
      ]
    : [];
}
export type UpstreamConnection = Omit<
  ConnectionReply,
  "http_status" | "checked_at"
> & {
  http_status: number | null;
  checked_at: string | null;
};
export type ViewState = {
  platform?: "macos" | "android";
  foreground?: boolean;
  initializing?: boolean;
  connection_revision?: number;
  connected: boolean;
  online: boolean;
  error: string;
  upstream_connection?: UpstreamConnection | null;
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
    connections: [],
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
export type QuotaBatch = Omit<
  WithRequired<
    WithNull<import("./protocol").QuotaRefreshHeader, "completed_at">,
    "id" | "total" | "completed" | "started_at" | "completed_at"
  >,
  "items"
> & {
  items: (Omit<
    WithRequired<
      import("./protocol").QuotaRefreshEntry,
      "account_id" | "account_name" | "platform" | "status"
    >,
    "usage"
  > & {
    usage?: UsageSummary;
  })[];
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

export function sortRecentCall(
  accounts: Account[],
  ascending = false,
): Account[] {
  const timestamp = (account: Account) => {
    const recent = callTime(account.last_called_at ?? null);
    return Number.isFinite(recent) ? recent : callTime(account.last_success_at);
  };
  return [...accounts].sort((a, b) => {
    const left = timestamp(a),
      right = timestamp(b);
    if (!Number.isFinite(left))
      return !Number.isFinite(right) ? b.id - a.id : 1;
    if (!Number.isFinite(right)) return -1;
    return (ascending ? 1 : -1) * (left - right) || b.id - a.id;
  });
}
