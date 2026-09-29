import type { RecordOption, UsageRecord } from "./records";
const now = Date.now();
const users: RecordOption[] = [
  {
    id: 1,
    name: "管理员",
    email: "admin@example.com",
    status: "active",
    deleted: false,
  },
  {
    id: 2,
    name: "工作空间",
    email: "studio@example.com",
    status: "active",
    deleted: false,
  },
  {
    id: 3,
    name: "历史用户",
    email: "history@example.com",
    status: "disabled",
    deleted: true,
  },
];
const keys: RecordOption[] = Array.from({ length: 125 }, (_, index) => {
  const user = users[index % 3];
  return {
    id: index + 1,
    name: index === 0 ? "codex-workspace" : `workspace-${index + 1}`,
    user_id: user.id,
    user_name: user.name,
    user_email: user.email,
    status: index % 7 ? "active" : "disabled",
    deleted: index > 119,
  };
});
const rows = Array.from(
  { length: 75 },
  (_, index): UsageRecord => ({
    id: 200 - index,
    created_at: new Date(now - index * 31000).toISOString(),
    user_id: (index % 2) + 1,
    user_name: users[index % 2].name,
    user_email: users[index % 2].email!,
    api_key_id: (index % 2) + 1,
    api_key_name: keys[index % 2].name,
    account_id: (index % 2) + 1,
    account_name: index % 2 ? "工作账户" : "个人账户",
    group_id: 1,
    group_name: "Codex",
    model: "gpt-6-astra",
    requested_model: "gpt-6-astra",
    upstream_model: index % 3 ? "gpt-6-astra" : "gpt-6-luna",
    upstream_response_model:
      index % 7 === 0
        ? "gpt-5.6-terra"
        : index % 3
          ? "gpt-6-astra"
          : "gpt-6-luna",
    model_mapping_chain: index % 3 ? null : "gpt-6-astra→gpt-6-luna",
    upstream_model_mismatch: index % 7 === 0,
    requested_reasoning_effort: "max",
    reasoning_effort: index % 7 ? "max" : "xhigh",
    request_type: index % 5 ? "stream" : "ws_v2",
    input_tokens: index ? 1177 + index * 53 : 3,
    output_tokens: index ? 1717 - index * 10 : 354,
    cache_read_tokens: index ? 127100 : 70600,
    cache_creation_tokens: index ? 0 : 12000,
    cache_creation_5m_tokens: 0,
    cache_creation_1h_tokens: 0,
    input_cost: "0.0002354000",
    output_cost: "0.0017170000",
    cache_read_cost: "0.0002946000",
    cache_creation_cost: "0.0000000000",
    total_cost: "0.0022470000",
    actual_cost: "0.0022470000",
    account_cost: "0.0022470000",
    account_stats_cost: null,
    account_rate_multiplier: "1",
    rate_multiplier: "1",
    first_token_ms: [3050, 10750, 34200, 67450, null][index % 5],
    duration_ms: [10950, 53790, 126650, 204500, 336800][index % 5],
    user_agent: "Codex Desktop/0.47 (Mac OS X; arm64)",
    ip_address: index % 2 ? "2001:db8:1:2:3:4:5:6789" : "192.0.2.10",
    inbound_endpoint: "/v1/responses",
    upstream_endpoint: "/backend-api/codex/responses",
    request_id: `req_preview_${200 - index}`,
    upstream_request_id: `upstream_preview_${200 - index}`,
    billing_type: 0,
    billing_mode: "token",
    service_tier: "priority",
    image_count: 0,
    image_input_tokens: 0,
    image_output_tokens: 0,
    image_input_cost: "0",
    image_output_cost: "0",
    video_count: 0,
    video_duration_seconds: null,
    video_resolution: null,
  }),
);
export function recordPreview(path: string) {
  const url = new URL(path, "https://preview.invalid");
  const p = url.searchParams;
  if (url.pathname === "/usage-record-options") {
    const q = (p.get("q") || "").toLowerCase();
    const directory = p.get("kind") === "users" ? users : keys;
    const filtered = directory.filter(
      (item) =>
        (!p.has("user_id") || item.user_id === Number(p.get("user_id"))) &&
        (!q ||
          item.name?.toLowerCase().includes(q) ||
          item.email?.toLowerCase().includes(q) ||
          item.id === Number(q.replace(/^#/, ""))) &&
        (!p.has("cursor") || item.id > Number(p.get("cursor"))),
    );
    const items = filtered.slice(0, Number(p.get("limit") || 50));
    return {
      items,
      next_cursor:
        filtered.length > items.length ? String(items.at(-1)!.id) : null,
    };
  }
  if (url.pathname !== "/usage-records") {
    const row = rows.find(
      (r) => r.id === Number(url.pathname.split("/").at(-1)),
    );
    if (!row) throw new Error("记录不存在");
    return structuredClone(row);
  }
  const filtered = rows.filter(
    (r) =>
      (!p.has("from_at") || r.created_at >= p.get("from_at")!) &&
      (!p.has("to_at") || r.created_at <= p.get("to_at")!) &&
      (!p.has("account_id") || r.account_id === Number(p.get("account_id"))) &&
      (!p.has("user_id") || r.user_id === Number(p.get("user_id"))) &&
      (!p.has("api_key_id") || r.api_key_id === Number(p.get("api_key_id"))) &&
      (!p.has("model") ||
        [r.model, r.upstream_model, r.upstream_response_model].some((m) =>
          m?.includes(p.get("model")!),
        )) &&
      (!p.has("request_type") || r.request_type === p.get("request_type")) &&
      (p.get("mismatch_only") !== "true" || r.upstream_model_mismatch),
  );
  if (p.has("after_id")) return { new_count: 0, latest_id: rows[0].id };
  const remaining = filtered.filter(
    (r) => !p.has("cursor") || r.id < Number(p.get("cursor")),
  );
  const items = remaining.slice(0, 50);
  return {
    items: structuredClone(items),
    next_cursor: remaining.length > 50 ? String(items.at(-1)!.id) : null,
    latest_id: rows[0].id,
    observed_at: new Date().toISOString(),
  };
}
