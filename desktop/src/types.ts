export type Account = {
  id: number;
  name: string;
  platform: string;
  type: string;
  status: string;
  schedulable: boolean;
  available: boolean;
  group_ids: number[];
  blockers: { code: string; label: string; until?: string }[];
  managed: boolean;
  version: string;
  last_success_at: string | null;
  last_error_at: string | null;
  last_error_id: number | null;
  last_error_code: string | null;
  last_error_status: number | null;
  error_message: string;
  success_after_error: boolean;
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
export type Incident = {
  account_id: number;
  account_name: string;
  platform: string;
  account_type: string;
  requested_model: string;
  upstream_model: string;
  response_model: string;
  status: string;
  action: string;
  reason: string;
  first_at: string;
  latest_at: string;
  count: number;
  history: boolean;
};
export type Snapshot = {
  observed_at: string;
  accounts: Account[];
  groups: Group[];
  errors: OpsError[];
  incidents: Incident[];
};
export type Preferences = {
  base_url: string;
  favorites: number[];
  pinned: boolean;
  launch_at_login: boolean;
};
export type ViewState = {
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
export function relativeTime(
  value: string | null | undefined,
  now = Date.now(),
): string {
  if (!value) return "暂无记录";
  const ms = Date.parse(value);
  if (!Number.isFinite(ms)) return "时间未知";
  const seconds = Math.max(0, Math.floor((now - ms) / 1000));
  if (seconds < 60) return `${seconds} 秒前`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}
export function fullTime(value: string | null | undefined): string {
  return value
    ? new Date(value).toLocaleString("zh-CN", {
        timeZone: "Asia/Shanghai",
        hour12: false,
      })
    : "时间未知";
}
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
