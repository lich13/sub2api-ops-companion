export type UsageRecord = {
  id: number;
  created_at: string;
  user_id: number;
  user_name: string | null;
  user_email: string | null;
  api_key_id: number;
  api_key_name: string | null;
  account_id: number;
  account_name: string | null;
  group_id: number | null;
  group_name: string | null;
  model: string | null;
  requested_model: string | null;
  upstream_model: string | null;
  upstream_response_model: string | null;
  model_mapping_chain: string | null;
  upstream_model_mismatch: boolean;
  reasoning_effort: string | null;
  requested_reasoning_effort: string | null;
  request_type: "sync" | "stream" | "ws_v2";
  native_request_type?: string;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_creation_tokens: number | null;
  cache_read_tokens: number | null;
  cache_creation_5m_tokens: number | null;
  cache_creation_1h_tokens: number | null;
  input_cost: string | null;
  output_cost: string | null;
  cache_creation_cost: string | null;
  cache_read_cost: string | null;
  total_cost: string | null;
  actual_cost: string | null;
  account_cost: string | null;
  account_stats_cost: string | null;
  account_rate_multiplier: string | null;
  rate_multiplier: string | null;
  first_token_ms: number | null;
  duration_ms: number | null;
  user_agent: string | null;
  ip_address: string | null;
  inbound_endpoint: string | null;
  upstream_endpoint: string | null;
  request_id: string | null;
  upstream_request_id: string | null;
  billing_type: number | null;
  billing_mode: string | null;
  service_tier: string | null;
  image_count: number | null;
  image_output_tokens: number | null;
  image_output_cost: string | null;
  image_input_tokens: number | null;
  image_input_cost: string | null;
  video_count: number | null;
  video_duration_seconds: number | null;
  video_resolution: string | null;
};
export type RecordPage = {
  items: UsageRecord[];
  next_cursor: string | null;
  latest_id: number;
  observed_at: string;
  summary?: { actual_cost: string };
};
export type RecordOption = {
  id: number;
  name: string | null;
  email?: string | null;
  user_id?: number | null;
  user_name?: string | null;
  user_email?: string | null;
  status?: string | null;
  deleted: boolean;
};
export type RecordOptionPage = {
  items: RecordOption[];
  next_cursor: string | null;
};
export const recordColumns = [
  ["api_key", "API 密钥"],
  ["account", "账户"],
  ["model", "模型"],
  ["reasoning", "推理强度"],
  ["tokens", "Token"],
  ["cost", "费用"],
  ["latency", "延迟"],
  ["user_agent", "User-Agent"],
  ["ip", "IP"],
  ["endpoint", "端点"],
  ["group", "分组"],
  ["billing", "计费模式"],
  ["request_id", "请求 ID"],
  ["upstream_id", "上游 ID"],
] as const;
export type RecordColumn = (typeof recordColumns)[number][0];
export const defaultRecordColumns: string[] = [
  "api_key",
  "account",
  "model",
  "reasoning",
  "tokens",
  "cost",
  "latency",
  "user_agent",
  "ip",
];
export function normalizeRecordColumns(columns: string[] | null | undefined) {
  return columns == null
    ? [...defaultRecordColumns]
    : columns.filter((column) => recordColumns.some(([key]) => key === column));
}
export const requestTypes = {
  sync: "非流式",
  stream: "流式",
  ws_v2: "WebSocket",
};
export function modelRoute(record: UsageRecord) {
  const steps: { model: string; labels: string[] }[] = [];
  const add = (model: string | null, label: string) => {
    if (!model?.trim()) return;
    const value = model.trim(),
      previous = steps.at(-1);
    if (previous?.model === value) {
      if (!previous.labels.includes(label)) previous.labels.push(label);
    } else steps.push({ model: value, labels: [label] });
  };
  add(record.requested_model || record.model, "请求");
  const chain =
    record.model_mapping_chain
      ?.split(/→|->/)
      .map((m) => m.trim())
      .filter(Boolean) ?? [];
  for (const model of chain) add(model, "映射");
  add(record.upstream_model || record.requested_model || record.model, "转发");
  add(record.upstream_response_model, "返回");
  return steps;
}
export function money(value: string | null | undefined) {
  if (value == null || value === "") return "—";
  // Decimal strings remain intact in the DTO/detail; rounding here is presentation only.
  const match = value.match(/^(-?)(\d+)(?:\.(\d*))?$/);
  if (!match)
    return Number.isFinite(Number(value))
      ? `$${Number(value).toFixed(6)}`
      : "—";
  const fraction = (match[3] || "").padEnd(7, "0");
  const rounded =
    BigInt(match[2]) * 1000000n +
    BigInt(fraction.slice(0, 6)) +
    (fraction[6] >= "5" ? 1n : 0n);
  return `$${match[1]}${rounded / 1000000n}.${(rounded % 1000000n).toString().padStart(6, "0")}`;
}
export function tokenCount(value: number | null | undefined) {
  return value == null || !Number.isFinite(value) || value < 0
    ? "—"
    : value.toLocaleString("en-US");
}
export function cacheTokenCount(value: number | null | undefined) {
  if (value == null || !Number.isFinite(value) || value < 0) return "—";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
  return tokenCount(value);
}
export function latency(value: number | null | undefined) {
  return value == null || !Number.isFinite(value) || value < 0
    ? "—"
    : `${(value / 1000).toFixed(2)}s`;
}
export function latencyTone(
  value: number | null | undefined,
  metric: "first" | "total",
) {
  if (value == null || !Number.isFinite(value) || value < 0) return "unknown";
  const [warn, slow, critical] =
    metric === "first" ? [10_000, 30_000, 60_000] : [60_000, 180_000, 300_000];
  return value >= critical
    ? "critical"
    : value >= slow
      ? "slow"
      : value >= warn
        ? "warn"
        : "good";
}
// Sub2API native display rule, pinned to 3a6fd1c9db07203ca308aaba69e502bc1f35b307:
// frontend/src/utils/latencyHealth.ts (formatUsageOutputRate).
// Use the full duration: output_tokens can include reasoning tokens.
export function formatUsageOutputRate(
  row: Pick<UsageRecord, "output_tokens" | "duration_ms" | "image_count" |
    "image_output_tokens" | "billing_mode"> & { request_type?: string | null; native_request_type?: string },
): string {
  const { output_tokens: output, duration_ms: total } = row;
  const kind = row.native_request_type ?? row.request_type;
  if ((row.image_count ?? 0) > 0 || (row.image_output_tokens ?? 0) > 0 ||
      row.billing_mode === "image" ||
      (kind && !["sync", "stream", "ws_v2", "cyber"].includes(kind)) ||
      output == null || !Number.isFinite(output) || output <= 0 ||
      total == null || !Number.isFinite(total) || total <= 0) return "—";
  return `${(output * 1000 / total).toFixed(1)} tok/s`;
}
