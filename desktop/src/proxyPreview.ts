import type { ProxyRow } from "./proxyData";
let nextId = 4;
let rows: ProxyRow[] = [
  {
    id: 1,
    name: "Primary",
    protocol: "socks5",
    host: "127.0.0.1",
    port: 1080,
    status: "active",
    username_set: false,
    password_set: false,
    account_count: 6,
    latency_ms: 208,
    quality_grade: "A",
    quality_score: 100,
    quality_status: "healthy",
    country: "示例地区",
    fallback_mode: "none",
    expiry_warn_days: 7,
  },
  {
    id: 2,
    name: "Backup",
    protocol: "https",
    host: "proxy.example.invalid",
    port: 8443,
    status: "active",
    username_set: true,
    password_set: true,
    account_count: 0,
    latency_ms: 520,
    quality_grade: "B",
    quality_score: 80,
    quality_status: "warn",
    expires_at: "2031-04-05T10:00:00Z",
    fallback_mode: "direct",
    expiry_warn_days: 7,
  },
  {
    id: 3,
    name: "IPv6",
    protocol: "socks5h",
    host: "::1",
    port: 1081,
    status: "inactive",
    username_set: false,
    password_set: false,
    account_count: 0,
  },
];
const credentials = new Map([
  [2, { username: "fixture", password: "fixture" }],
]);
export function proxyPreview(
  method: string,
  path: string,
  body: Record<string, unknown>,
) {
  const url = new URL(path, "https://example.invalid"),
    q = url.searchParams,
    part = url.pathname.split("/").slice(2),
    id = Number(part[0]);
  const row = rows.find((r) => r.id === id);
  let filtered = rows.filter(
    (r) =>
      (!q.get("search") ||
        `${r.name} ${r.host}`
          .toLowerCase()
          .includes(q.get("search")!.toLowerCase())) &&
      (!q.get("protocol") || r.protocol === q.get("protocol")) &&
      (!q.get("status") || r.status === q.get("status")),
  );
  if (q.has("ids"))
    filtered = filtered.filter((r) =>
      q.get("ids")!.split(",").includes(String(r.id)),
    );
  if (part[0] === "data") {
    if (method === "GET")
      return {
        exported_at: new Date().toISOString(),
        proxies: filtered.map((r) => ({
          name: r.name,
          protocol: r.protocol,
          host: r.host,
          port: r.port,
          status: r.status,
          ...credentials.get(r.id),
        })),
        accounts: [],
      };
    const data = body.data as { proxies: Record<string, unknown>[] };
    data.proxies.forEach((p) => create(p));
    return {
      proxy_created: data.proxies.length,
      proxy_reused: 0,
      proxy_failed: 0,
      errors: [],
    };
  }
  if (part[0] === "batch") {
    const values = body.proxies as Record<string, unknown>[];
    values.forEach((p) => create(p));
    return { created: values.length, skipped: 0 };
  }
  if (part[0] === "batch-delete") {
    const ids = body.ids as number[],
      skipped = rows
        .filter((r) => ids.includes(r.id) && (r.account_count ?? 0) > 0)
        .map((r) => ({ id: r.id, reason: "代理仍有关联账号" })),
      deleted_ids = ids.filter((id) => !skipped.some((r) => r.id === id));
    rows = rows.filter((r) => !deleted_ids.includes(r.id));
    return { deleted_ids, skipped };
  }
  if (!part.length) {
    if (method === "POST") return create(body);
    const key = (q.get("sort_by") || "id") as keyof ProxyRow;
    filtered.sort(
      (a, b) =>
        String(a[key] ?? "").localeCompare(String(b[key] ?? ""), undefined, {
          numeric: true,
        }) * (q.get("sort_order") === "asc" ? 1 : -1),
    );
    const page = Number(q.get("page") || 1),
      size = Number(q.get("page_size") || 20);
    return structuredClone({
      items: filtered.slice((page - 1) * size, page * size),
      page,
      page_size: size,
      total: filtered.length,
      pages: Math.ceil(filtered.length / size),
    });
  }
  if (!row) throw new Error("代理不存在");
  if (part[1] === "credentials")
    return credentials.get(id) || { username: "", password: "" };
  if (part[1] === "accounts")
    return (row.account_count ?? 0) > 0
      ? [{ id: 1, name: "Fixture account", platform: "openai", type: "oauth" }]
      : [];
  if (method === "DELETE") {
    if (row.account_count) throw new Error("代理仍有关联账号");
    rows = rows.filter((r) => r.id !== id);
    credentials.delete(id);
    return { deleted: true };
  }
  if (method === "PUT") return update(row, body);
  return structuredClone(row);
}
function update(row: ProxyRow, body: Record<string, unknown>) {
  const { username, password, expires_at, ...fields } = body;
  Object.assign(row, fields);
  if ("expires_at" in body)
    row.expires_at = expires_at
      ? new Date(Number(expires_at) * 1000).toISOString()
      : null;
  const prior = credentials.get(row.id) || { username: "", password: "" };
  if (typeof username === "string") prior.username = username;
  if (typeof password === "string") prior.password = password;
  credentials.set(row.id, prior);
  row.username_set = !!prior.username;
  row.password_set = !!prior.password;
  return structuredClone(row);
}
function create(body: Record<string, unknown>) {
  const row = {
    id: nextId++,
    name: "default",
    status: "active",
    account_count: 0,
  } as ProxyRow;
  update(row, body);
  rows.push(row);
  return structuredClone(row);
}
