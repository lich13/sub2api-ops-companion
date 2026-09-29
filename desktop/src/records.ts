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
};
export const recordColumns = [
  ["api_key", "API 密钥"],
  ["account", "账户"],
  ["model", "模型"],
  ["reasoning", "推理强度"],
  ["type", "类型"],
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
export const defaultRecordColumns: string[] = recordColumns
  .slice(0, 10)
  .map(([key]) => key);
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
  return value == null ? "—" : value.toLocaleString("en-US");
}
export function latency(value: number | null | undefined) {
  return value == null ? "—" : `${(value / 1000).toFixed(2)}s`;
}
