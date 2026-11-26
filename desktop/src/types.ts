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
  QuotaRefreshEntry,
  QuotaRefreshHeader,
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

export type WireNull<
  T,
  Req extends keyof T = never,
  Opt extends keyof T = never,
> = Omit<T, Req | Opt> & {
  [P in Req]-?: NonNullable<T[P]> | null;
} & {
  [P in Opt]?: NonNullable<T[P]> | null;
};
export type WireReq<T, K extends keyof T> = Omit<T, K> & {
  [P in K]-?: NonNullable<T[P]>;
};

export type DegradationMark = WireNull<
  DegradationMarkRecord,
  never,
  "marked_at"
>;
export type AccountKeepalive = WireNull<
  KeepaliveReply,
  "last_restore_at" | "next_allowed_at"
>;
export type ModelDetectionSummary = WireNull<
  ModelDetectionSummaryRecord,
  never,
  | "effective_interval_minutes"
  | "recent_request_count"
  | "last_automatic_started_at"
  | "next_at"
  | "last_result"
>;
export type UsageAction = UsageSummaryRecord["actions"][number];
export type QuotaPlan = WireNull<
  QuotaPlanRecord,
  "remaining" | "used" | "total" | "reset_at"
>;
export type KeyQuota = Omit<
  WireNull<
    KeyQuotaRecord,
    | "source"
    | "checked_at"
    | "success_at"
    | "retry_at"
    | "next_refresh_at"
    | "error"
    | "key_status"
    | "expires_at"
    | "expires_at_unix"
  >,
  "plans" | "subscriptions" | "today" | "total_usage"
> & {
  plans: QuotaPlan[];
  subscriptions: (Omit<
    WireNull<
      QuotaSubscriptionRecord,
      "name" | "status" | "active" | "expires_at"
    >,
    "plans"
  > & {
    plans: QuotaPlan[];
  })[];
  today: WireNull<KeyQuotaUsageRecord, keyof KeyQuotaUsageRecord> | null;
  total_usage: WireNull<KeyQuotaUsageRecord, keyof KeyQuotaUsageRecord> | null;
};
export type UsageWindow = WireNull<
  UsageWindowRecord,
  "used_percent" | "reset_at" | "observed_at",
  "used" | "limit" | "remaining" | "stats" | "estimated_total_cost"
>;
export type UsageSummary = Omit<
  WireNull<
    UsageSummaryRecord,
    "today",
    "prepaid_balance" | "monthly_limit" | "monthly_used"
  >,
  "key_quota" | "balances" | "windows" | "reset_credits"
> & {
  key_quota?: KeyQuota;
  balances?: WireNull<UsageBalanceRecord, never, "observed_at">[];
  windows: UsageWindow[];
  reset_credits: WireNull<
    ResetCreditsInfoRecord,
    "available" | "observed_at"
  > | null;
};
export type Account = Omit<
  WireNull<
    WireReq<AccountRecord, "status" | "error_message" | "success_after_error">,
    | "last_success_at"
    | "last_error_at"
    | "last_error_id"
    | "last_error_code"
    | "last_error_status",
    "parent_account_id" | "concurrency" | "load_factor" | "last_called_at"
  >,
  | "degradation_mark"
  | "model_detection"
  | "blockers"
  | "usage_windows"
  | "usage"
  | "recovery_monitor"
  | "auto_reset_credit"
> & {
  degradation_mark?: DegradationMark;
  model_detection?: ModelDetectionSummary;
  blockers: AccountBlockerRecord[];
  usage_windows: UsageWindow[];
  usage?: UsageSummary;
  recovery_monitor?: WireNull<
    RecoveryMonitorStateRecord,
    never,
    "next_at" | "next_retry_at"
  > | null;
  auto_reset_credit?: WireNull<
    AutoResetCreditStateRecord,
    never,
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
  > | null;
};
export type RecentAccount = WireNull<RecentAccountRecord, "called_at">;
export type Group = Omit<
  WireNull<GroupRecord, "account_id" | "called_at">,
  "recent_accounts"
> & {
  recent_accounts: RecentAccount[];
};
function callTime(value: string | null): number {
  const parsed = value ? Date.parse(value) : NaN;
  return Number.isFinite(parsed) ? parsed : Number.NEGATIVE_INFINITY;
}

export function currentGroups(groups: Group[], accounts: Account[]): Group[] {
  const memberGroups = new Map(
    accounts.map((account) => [account.id, new Set(account.group_ids)]),
  );
  return groups
    .map((group) => {
      const ordered = group.recent_accounts
        .filter((call) => memberGroups.get(call.account_id)?.has(group.id))
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
  WireNull<
    WireReq<
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
    "account_id" | "group_id" | "upstream_status_code"
  >,
  "notification"
> & {
  notification?: WireNull<
    NonNullable<OpsErrorRecord["notification"]>,
    never,
    "next_at"
  >;
};
export type Recovery = Omit<
  WireNull<
    WireReq<
      RecoveryRecord,
      "id" | "account_id" | "account_name" | "model_id" | "legacy"
    >,
    "test_completed_at" | "recovered_at"
  >,
  "reset_credit"
> & {
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
export type UpstreamConnection = WireNull<
  ConnectionReply,
  "http_status" | "checked_at"
>;
export type ViewState = {
  platform?: "macos" | "android";
  foreground?: boolean;
  initializing?: boolean;
  connection_revision?: number;
  snapshot_revision?: number;
  connected: boolean;
  online: boolean;
  error: string;
  upstream_connection?: UpstreamConnection | null;
  snapshot: Snapshot | null;
  preferences: Preferences;
};
export function connectionIdentity(
  state: Pick<ViewState, "preferences" | "connected" | "connection_revision">,
): string {
  return `${state.preferences.base_url}:${state.connected}:${state.connection_revision ?? 0}`;
}
export type AppUpdatePhase =
  | "idle"
  | "checking"
  | "up_to_date"
  | "downloading"
  | "verifying"
  | "permission_required"
  | "installing"
  | "error";
export type AppUpdateSnapshot = {
  phase: AppUpdatePhase;
  current_version: string;
  latest_version: string | null;
  downloaded_bytes: number;
  total_bytes: number | null;
  message: string;
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
const FULL_TIME_CACHE_LIMIT = 1024;
const fullTimeCache = new Map<string, string>();
export function fullTime(value: string | null | undefined): string {
  if (!value) return "暂无记录";
  const cached = fullTimeCache.get(value);
  if (cached !== undefined) return cached;
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "时间未知";
  const parts = Object.fromEntries(
    timeFormatter.formatToParts(date).map(({ type, value }) => [type, value]),
  );
  const formatted = `${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`;
  if (fullTimeCache.size >= FULL_TIME_CACHE_LIMIT) {
    const oldest = fullTimeCache.keys().next().value;
    if (oldest !== undefined) fullTimeCache.delete(oldest);
  }
  fullTimeCache.set(value, formatted);
  return formatted;
}
export function sortPriority(accounts: Account[], ascending = true): Account[] {
  return [...accounts].sort(
    (a, b) => (ascending ? 1 : -1) * (a.priority - b.priority) || a.id - b.id,
  );
}
export type QuotaBatch = Omit<
  WireNull<
    WireReq<QuotaRefreshHeader, "id" | "total" | "completed" | "started_at">,
    "completed_at"
  >,
  "items"
> & {
  items: (Omit<
    WireReq<
      QuotaRefreshEntry,
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
  const normalizedQuery = query ? query.toLowerCase() : "";
  const groupId = group ? Number(group) : NaN;
  return accounts.filter(
    (a) =>
      (!normalizedQuery ||
        `${a.id} ${a.name}`.toLowerCase().includes(normalizedQuery)) &&
      (!group || a.group_ids.includes(groupId)) &&
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
  if (accounts.length <= 1) return [...accounts];
  const keyed = accounts.map((account) => {
    const recent = callTime(account.last_called_at ?? null);
    const ts = Number.isFinite(recent)
      ? recent
      : callTime(account.last_success_at);
    return { account, ts };
  });
  return keyed
    .sort((lhs, rhs) => {
      const left = lhs.ts,
        right = rhs.ts;
      if (!Number.isFinite(left))
        return !Number.isFinite(right) ? rhs.account.id - lhs.account.id : 1;
      if (!Number.isFinite(right)) return -1;
      return (
        (ascending ? 1 : -1) * (left - right) || rhs.account.id - lhs.account.id
      );
    })
    .map((entry) => entry.account);
}
