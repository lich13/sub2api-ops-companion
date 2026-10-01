import type { Account } from "./types";

const original = { configured: true, version: "1".repeat(64), normal: { whitelist: ["gpt-6-sol", "gpt-6-astra", "gpt-6.1-sol"], mappings: [] as { source: string; target: string }[] }, degraded: { whitelist: ["gpt-6-luna"], mappings: [{ source: "gpt-6-astra", target: "gpt-6-luna" }] } };
let config = structuredClone(original);
let items: { account_id: number; name: string; marked: boolean; before: typeof original.normal; after: typeof original.normal; status: string; unrestricted: boolean }[] = [];
const jobs = new Map<string, { id: string; items: typeof items }>();
export function profilePreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[]) {
  if (path === "/account-model-profiles" && method === "GET") return structuredClone(config);
  if (path === "/account-model-profiles" && method === "PUT") {
    if (body.expected_version !== config.version) throw new Error("模板已变化，请重新读取");
    config = { ...config, normal: body.normal as typeof config.normal, degraded: body.degraded as typeof config.degraded, version: Date.now().toString(16).padEnd(64, "0") };
    return structuredClone(config);
  }
  if (path.endsWith("/preview")) {
    items = accounts.filter((a) => a.platform === "openai" && a.type === "oauth").map((a) => ({ account_id: a.id, name: a.name, marked: !!a.degradation_mark?.marked, before: { whitelist: ["gpt-5.6-luna"], mappings: [] }, after: structuredClone(a.degradation_mark?.marked ? config.degraded : config.normal), status: "queued", unrestricted: !(a.degradation_mark?.marked ? config.degraded : config.normal).whitelist.length && !(a.degradation_mark?.marked ? config.degraded : config.normal).mappings.length }));
    return { items: structuredClone(items), version: config.version };
  }
  if (path.endsWith("/apply")) {
    const id = crypto.randomUUID().replaceAll("-", "");
    const job = { id, items: items.map((item) => ({ ...item, status: "applied" })) };
    jobs.set(id, job); return structuredClone(job);
  }
  return structuredClone(jobs.get(path.split("/").at(-1)!) ?? null);
}
