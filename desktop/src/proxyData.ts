import { api, preview } from "./bridge";
export type ProxyRow = {
  id: number;
  name: string;
  protocol: string;
  host: string;
  port: number;
  status: string;
  username_set: boolean;
  password_set: boolean;
  created_at?: string | null;
  expires_at?: string | null;
  fallback_mode?: string | null;
  backup_proxy_id?: number | null;
  expiry_warn_days?: number | null;
  account_count?: number | null;
  latency_ms?: number | null;
  latency_status?: string | null;
  ip_address?: string | null;
  country?: string | null;
  country_code?: string | null;
  region?: string | null;
  city?: string | null;
  quality_status?: string | null;
  quality_score?: number | null;
  quality_grade?: string | null;
  quality_checked?: number | null;
};
export type ProxyInput = {
  name: string;
  protocol: string;
  host: string;
  port: number;
  username?: string;
  password?: string;
  status?: string;
  expires_at?: number | null;
  fallback_mode?: string;
  backup_proxy_id?: number | null;
  expiry_warn_days?: number;
};
export type ProxyPage = {
  items: ProxyRow[];
  page: number;
  page_size: number;
  total: number;
  pages: number;
};
export type ProxyCredentials = { username: string; password: string };
export type ProxyAccount = {
  id: number;
  name: string;
  platform: string;
  type: string;
};
export type ConnectionResult = {
  success: boolean;
  message: string;
  latency_ms: number | null;
  ip_address: string;
  city: string;
  region: string;
  country: string;
  country_code: string;
};
export type QualityResult = {
  score: number;
  grade: string;
  summary: string;
  exit_ip: string;
  country: string;
  base_latency_ms: number | null;
  checked_at?: number;
  items: {
    target: string;
    status: "pass" | "warn" | "fail" | "challenge";
    http_status: number | null;
    latency_ms: number | null;
    message: string;
  }[];
};
export type ImportData = {
  exported_at?: string;
  type?: string;
  version?: number;
  proxies: unknown[];
  accounts?: never[];
};
export type ImportResult = {
  proxy_created: number;
  proxy_reused: number;
  proxy_failed: number;
  errors: { name: string; message: string }[];
};
export const proxyAddress = (p: Pick<ProxyRow, "host" | "port">) =>
  `${p.host.includes(":") ? `[${p.host.replace(/^\[|\]$/g, "")}]` : p.host}:${p.port}`;
export function proxyUrl(p: ProxyRow, credentials?: ProxyCredentials) {
  const auth =
    credentials?.username || credentials?.password
      ? `${encodeURIComponent(credentials!.username)}:${encodeURIComponent(credentials!.password)}@`
      : "";
  return `${p.protocol}://${auth}${proxyAddress(p)}`;
}
export function parseProxyLines(text: string) {
  const proxies: Omit<ProxyInput, "name">[] = [],
    errors: number[] = [],
    seen = new Set<string>();
  let duplicates = 0;
  text.split(/\r?\n/).forEach((raw, i) => {
    const line = raw.trim();
    if (!line) return;
    try {
      const match =
        /^(https?|socks5h?):\/\/(?:([^@]*)@)?(\[[^\]]+\]|[^:/?#\s]+):(\d+)$/i.exec(
          line,
        );
      if (!match) throw new Error();
      const port = Number(match[4]);
      if (!Number.isInteger(port) || port < 1 || port > 65535)
        throw new Error();
      const auth = match[2]?.split(":") ?? [],
        username = decodeURIComponent(auth.shift() ?? ""),
        password = decodeURIComponent(auth.join(":"));
      const row = {
        protocol: match[1].toLowerCase(),
        host: match[3].replace(/^\[|\]$/g, ""),
        port,
        username,
        password,
      };
      const key = JSON.stringify(row);
      if (seen.has(key)) {
        duplicates++;
        return;
      }
      seen.add(key);
      proxies.push(row);
    } catch {
      errors.push(i + 1);
    }
  });
  return { proxies, errors, duplicates };
}
export function parseImport(text: string): ImportData {
  if (text.length > 8_000_000) throw new Error("文件超过 8 MB");
  let data: ImportData;
  try {
    data = JSON.parse(text) as ImportData;
  } catch {
    throw new Error("JSON 格式无效");
  }
  if (
    !data ||
    !Array.isArray(data.proxies) ||
    data.proxies.length === 0 ||
    data.proxies.length > 1000
  )
    throw new Error("请选择包含 1–1000 个代理的 Sub2API JSON 文件");
  if (data.accounts && (!Array.isArray(data.accounts) || data.accounts.length))
    throw new Error("仅支持代理数据，请从网页端 IP 管理导出");
  return data;
}
export async function readProxyFile(): Promise<string | null> {
  if (preview)
    return new Promise((resolve, reject) => {
      const input = document.createElement("input");
      input.type = "file";
      input.accept = ".json,application/json";
      input.onchange = () => {
        const file = input.files?.[0];
        if (!file) resolve(null);
        else void file.text().then(resolve, reject);
      };
      input.oncancel = () => resolve(null);
      input.click();
    });
  const [{ open }, { readTextFile }] = await Promise.all([
    import("@tauri-apps/plugin-dialog"),
    import("@tauri-apps/plugin-fs"),
  ]);
  const path = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "JSON", extensions: ["json"] }],
  });
  return path ? readTextFile(path) : null;
}
export async function exportProxyFile(
  path: string,
  valid: () => boolean = () => true,
): Promise<boolean> {
  if (preview) {
    const data = await api<ImportData>("GET", path);
    if (!valid()) return false;
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }),
    );
    const a = document.createElement("a");
    a.href = url;
    a.download = "sub2api-proxies.json";
    a.click();
    URL.revokeObjectURL(url);
    return true;
  }
  const [{ save }, { writeTextFile }] = await Promise.all([
    import("@tauri-apps/plugin-dialog"),
    import("@tauri-apps/plugin-fs"),
  ]);
  const target = await save({
    defaultPath: "sub2api-proxies.json",
    filters: [{ name: "JSON", extensions: ["json"] }],
  });
  if (!target || !valid()) return false;
  const data = await api<ImportData>("GET", path);
  if (!valid()) return false;
  await writeTextFile(target, JSON.stringify(data, null, 2));
  return true;
}
