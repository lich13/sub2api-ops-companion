// Imported only by the development preview transport. No production credentials.
type Entry = {
  model: string;
  efforts: string[];
  default_effort: string;
  state: string;
  reason: string;
  source: string;
  updated_at: string;
};
const groups = [
  { id: 1, name: "Codex · 主力", platform: "openai", version: "a".repeat(64) },
  { id: 2, name: "Grok · 开发", platform: "grok", version: "b".repeat(64) },
];
const items: Record<number, Entry[]> = { 1: [], 2: [] };
const allowed = new Set<string>();
let serial = 0;
let revision = "c".repeat(64);
const state = (id: number) => ({
  group: groups.find((g) => g.id === id) || groups[0],
  revision,
  items: items[id] || [],
  status: { state: "ready", message: "" },
});
export function modelPreview(
  method: string,
  path: string,
  payload: Record<string, unknown>,
) {
  if (path === "/model-groups")
    return { groups, status: { state: "ready", message: "" } };
  const id = Number(path.split("/")[2]);
  const group = state(id).group;
  const model = String(payload.model || "");
  const limited = id === 2;
  if (path.endsWith("/resolve"))
    return {
      group,
      revision,
      model,
      binding: "d".repeat(64),
      efforts: payload.efforts || ["low", "medium", "high"],
      default_effort: payload.default_effort || "medium",
      source: payload.efforts ? "manual" : "upstream",
      needs_allowlist: id === 1 && !allowed.has(model),
      descriptor_available: true,
      native_efforts: [],
      native_default: "",
      forwarding: {
        state: limited ? "limited" : "verified",
        reason: limited
          ? "原版 Sub2API 会移除该 Grok 模型的思考强度"
          : "当前版本可保留所选 Responses 思考档位",
        version: "0.2.10",
      },
    };
  if (method === "PUT") {
    if (!limited && payload.confirm_allowlist) allowed.add(model);
    items[id] = (items[id] || []).filter((m) => m.model !== model);
    items[id].push({
      model,
      efforts: payload.efforts as string[],
      default_effort: String(payload.default_effort),
      state: limited ? "limited" : "active",
      reason: limited ? "原版转发受限" : "目录已补全",
      source: "upstream",
      updated_at: new Date().toISOString(),
    });
    revision = String(++serial).padStart(64, "0");
    return {
      outcome: limited ? "draft" : "saved",
      message: limited ? "已保存草稿，未对外发布" : "目录已补全",
      state: state(id),
    };
  }
  if (method === "DELETE")
    items[id] = (items[id] || []).filter((m) => m.model !== model);
  return state(id);
}
