import catalog from "../../server/src/account_creation_fields.json";

export type RecordValue = Record<string, unknown>;
export type CreationDraft = {
  name: string; notes: string | null; platform: string; type: string;
  proxy_id: number | null; concurrency: number; load_factor: number | null;
  priority: number; rate_multiplier: number; group_ids: number[]; expires_at: number | null;
  credentials: RecordValue; extra: RecordValue; auto_pause_on_expired?: boolean;
  upstream_billing_probe_enabled?: boolean;
};
export type Field = {
  path: string; label: string; kind: string; group: string; platforms?: string[];
  types?: string[]; default?: unknown; choices?: string[]; when?: RecordValue; min?: number; max?: number;
};
export type CreationOptions = {
  groups: { id: number; name: string; platform: string; status?: string; long_context_pricing_enabled?: boolean }[];
  proxies: { id: number; name: string; status?: string }[];
};
export type Created = { account: { id: number; name: string; platform: string; type: string }; warnings: string[] };
export const fields = catalog.fields as Field[];
export const platforms = catalog.platforms;
export const typeNames: Record<string, string> = { oauth: "OAuth", "setup-token": "Setup Token", apikey: "API Key", bedrock: "Bedrock", service_account: "Vertex Service Account" };
export const labels: Record<string, string> = {
  sigv4: "AWS Signature V4", apikey: "API Key", payg: "按量付费", coding: "Coding Plan", zen: "Zen", go: "Go",
  adaptive: "自动选择协议", chat_completions: "Chat Completions", anthropic: "Anthropic", responses: "Responses",
  code_assist: "GCP Code Assist", google_one: "Google One", ai_studio: "AI Studio",
  google_one_free: "Google One 免费", google_ai_pro: "Google AI Pro", google_ai_ultra: "Google AI Ultra",
  gcp_standard: "GCP Standard", gcp_enterprise: "GCP Enterprise", aistudio_free: "AI Studio 免费", aistudio_paid: "AI Studio 付费",
  rolling: "滚动窗口", fixed: "固定值", percentage: "百分比", off: "关闭", device: "设备", session: "会话", full: "完整",
  auto: "自动", force_on: "强制开启", force_off: "强制关闭", force_responses: "强制 Responses", force_chat_completions: "强制 Chat Completions",
  ctx_pool: "上下文池", passthrough: "透传", http_bridge: "HTTP Bridge", enabled: "开启", disabled: "关闭", default: "跟随全局",
  x_api_key: "X-API-Key", authorization_bearer: "Authorization Bearer", tiered: "分层", sticky_exempt: "粘性豁免", throttle: "节流", serialize: "串行", embeddings: "Embeddings", images: "Images",
};
export function getField(draft: CreationDraft, path: string): unknown {
  const [section, key] = path.split(".");
  return key ? (draft[section as "credentials" | "extra"] as RecordValue)[key] : draft[section as keyof CreationDraft];
}
export function setField(draft: CreationDraft, path: string, value: unknown): CreationDraft {
  const [section, key] = path.split(".");
  return key ? { ...draft, [section]: { ...draft[section as "credentials" | "extra"], [key]: value } } : { ...draft, [section]: value };
}
export function applicable(f: Field, draft: Pick<CreationDraft, "platform" | "type">) {
  return (!f.platforms || f.platforms.includes(draft.platform)) && (!f.types || f.types.includes(draft.type));
}
export function fieldVisible(f: Field, draft: CreationDraft) {
  if (!applicable(f, draft)) return false;
  if (f.path === "credentials.model_mapping" && draft.platform === "openai" && draft.extra.openai_passthrough === true) return false;
  if (f.path === "credentials.base_url" && multiProtocol(draft.platform) && draft.credentials.api_protocol === "adaptive") return false;
  if (f.path === "extra.openai_responses_mode" && Array.isArray(draft.credentials.openai_capabilities) && !draft.credentials.openai_capabilities.includes("chat_completions")) return false;
  if (f.path === "extra.quota_reset_timezone" && draft.extra.quota_daily_reset_mode !== "fixed" && draft.extra.quota_weekly_reset_mode !== "fixed") return false;
  return !f.when || Object.entries(f.when).every(([key, value]) => getField(draft, key) === value);
}
export function availableGroups(draft: CreationDraft, groups: CreationOptions["groups"]) {
  return groups.filter(g => g.platform === draft.platform || g.platform === "composite" || draft.platform === "antigravity" && draft.extra.mixed_scheduling === true && ["anthropic", "gemini"].includes(g.platform));
}
export function groupLongContextBilling(draft: CreationDraft, groups: CreationOptions["groups"]) {
  return draft.group_ids.length > 0 && draft.group_ids.every(id => groups.some(g => g.id === id && g.long_context_pricing_enabled === true));
}
export const isOAuth = (draft: CreationDraft) => ["oauth", "setup-token"].includes(draft.type);
export const multiProtocol = (p: string) => ["kimi", "zhipu", "deepseek", "minimax", "opencode_go"].includes(p);
export function protocols(platform: string) { return platform === "zhipu" ? ["chat_completions", "anthropic"] : ["chat_completions", "anthropic", "responses"]; }
export function tierOptions(draft: CreationDraft) {
  const type = draft.type === "apikey" ? "ai_studio" : draft.credentials.oauth_type;
  return type === "google_one" ? ["google_one_free", "google_ai_pro", "google_ai_ultra"] : type === "code_assist" ? ["gcp_standard", "gcp_enterprise"] : ["aistudio_free", "aistudio_paid"];
}
export function defaultUrl(platform: string, mode: string, protocol: string) {
  if (platform === "opencode_go") return `https://opencode.ai/zen${mode === "go" ? "/go" : ""}${protocol === "anthropic" ? "" : "/v1"}`;
  const options = catalog.url_presets[platform as keyof typeof catalog.url_presets] ?? [];
  return options.find(p => p.mode === mode && p.protocol === protocol)?.url ?? options.find(p => p.protocol === protocol)?.url ?? platforms.find(p => p.id === platform)?.base_url ?? "";
}
export function withProtocolDefaults(draft: CreationDraft): CreationDraft {
  if (!multiProtocol(draft.platform)) return draft;
  const mode = String(draft.credentials.account_mode), protocol = String(draft.credentials.api_protocol);
  const urls = Object.fromEntries(protocols(draft.platform).map(p => [p, defaultUrl(draft.platform, mode, p)]));
  return { ...draft, credentials: { ...draft.credentials, base_url: defaultUrl(draft.platform, mode, protocol), api_base_urls: urls,
    ...(draft.platform === "opencode_go" ? { protocol_rules: structuredClone(catalog.protocol_rules[mode === "go" ? "go" : "zen"]) } : {}) } };
}
export function newDraft(platform = "anthropic", type?: string): CreationDraft {
  const p = platforms.find(p => p.id === platform) ?? platforms[0];
  const kind = type && p.types.includes(type) ? type : p.types[0];
  let value: CreationDraft = { name: "", notes: "", platform: p.id, type: kind, credentials: {}, extra: {}, proxy_id: null, concurrency: p.id === "grok" ? 1 : 10, load_factor: null, priority: 1, rate_multiplier: 1, group_ids: [], expires_at: null };
  for (const f of fields.filter(f => applicable(f, value))) if (f.default !== undefined) value = setField(value, f.path, structuredClone(f.default));
  if (kind === "apikey") value.credentials.base_url = p.base_url;
  if (p.id === "gemini") value.credentials.tier_id = tierOptions(value)[0];
  if (fields.some(f => f.path === "credentials.model_mapping" && applicable(f,value)) && !["grok", "antigravity"].includes(p.id)) value.credentials.model_mapping = Object.fromEntries(modelPresets(p.id).map(m => [m,m]));
  return withProtocolDefaults(value);
}
export function modelPresets(platform: string): string[] { return catalog.models[platform as keyof typeof catalog.models] ?? []; }
export function mappingPresets(draft: CreationDraft) { const key = draft.type === "bedrock" ? "bedrock" : draft.platform; return catalog.mapping_presets[key as keyof typeof catalog.mapping_presets] ?? []; }
export function urlPresets(draft: CreationDraft) { return (catalog.url_presets[draft.platform as keyof typeof catalog.url_presets] ?? []).filter(p => draft.platform === "grok" || p.mode === draft.credentials.account_mode && (draft.credentials.api_protocol === "adaptive" || p.protocol === draft.credentials.api_protocol)); }

export function reuseDraft(current: CreationDraft, source: CreationDraft): CreationDraft {
  if (source.platform !== current.platform || source.type !== current.type) throw new Error("来源账号平台或类型已变化，请重新选择");
  let result: CreationDraft = { ...current, credentials: { ...current.credentials }, extra: {} };
  for (const key of ["notes", "proxy_id", "concurrency", "load_factor", "priority", "rate_multiplier", "group_ids", "expires_at", "auto_pause_on_expired"] as const) {
    if (Object.prototype.hasOwnProperty.call(source, key)) result = { ...result, [key]: structuredClone(source[key]) };
  }
  // Replace configuration, while preserving the new account's identity inputs.
  for (const f of fields.filter(f => applicable(f,current))) {
    const value = getField(source, f.path);
    result = setField(result, f.path, value === undefined ? undefined : structuredClone(value));
  }
  return result;
}
function listNumbers(value: unknown) {
  if (typeof value === "string") return value.trim() ? value.split(/[,，\s]+/).filter(Boolean).map(Number) : [];
  return value;
}
export function customErrorRisks(draft: CreationDraft): number[] {
  if(draft.type!=="apikey" || draft.credentials.custom_error_codes_enabled!==true)return [];
  const codes=listNumbers(draft.credentials.custom_error_codes);
  return [429,529].filter(code=>Array.isArray(codes) && codes.includes(code));
}
export function buildPayload(draft: CreationDraft, identity: RecordValue = {}, identityExtra: RecordValue = {}): CreationDraft {
  let value: CreationDraft = { name: draft.name.trim(), notes: draft.notes, platform: draft.platform, type: draft.type, proxy_id: draft.proxy_id,
    concurrency: draft.concurrency, load_factor: draft.load_factor, priority: draft.priority, rate_multiplier: draft.rate_multiplier,
    group_ids: [...draft.group_ids], expires_at: draft.expires_at, credentials: {}, extra: { ...identityExtra } };
  for (const f of fields.filter(f => fieldVisible(f,draft))) {
    let v = getField(draft,f.path);
    if (v === undefined || v === "" && f.kind !== "select") continue;
    if (f.kind === "numbers") v = listNumbers(v);
    if (f.kind === "stringbool" && typeof v === "boolean") v = String(v);
    value = setField(value,f.path,structuredClone(v));
  }
  const secretKeys = ["api_key", "aws_access_key_id", "aws_secret_access_key", "aws_session_token", "service_account_json"];
  for (const key of secretKeys) if (draft.credentials[key]) value.credentials[key] = String(draft.credentials[key]).trim();
  if (draft.type === "service_account") {
    const service = JSON.parse(String(value.credentials.service_account_json || "{}")) as RecordValue;
    if (!service.private_key || !service.client_email || !service.project_id) throw new Error("Service Account 文件缺少私钥、邮箱或项目 ID");
    Object.assign(value.credentials, { client_email: service.client_email, project_id: service.project_id, tier_id: "vertex" });
  }
  if (draft.type === "bedrock" && value.credentials.auth_mode === "sigv4") delete value.credentials.api_key;
  if (draft.type === "bedrock" && value.credentials.auth_mode === "apikey") for (const key of ["aws_access_key_id", "aws_secret_access_key", "aws_session_token"]) delete value.credentials[key];
  if (multiProtocol(draft.platform) && value.credentials.api_protocol === "adaptive") value.credentials.base_url = (value.credentials.api_base_urls as RecordValue)?.chat_completions;
  Object.assign(value.credentials, identity);
  if (draft.platform === "openai") {
    const key = `openai_${draft.type}_responses_websockets_v2_mode`;
    value.extra[`openai_${draft.type}_responses_websockets_v2_enabled`] = !!value.extra[key] && value.extra[key] !== "off";
  }
  return value;
}
export function validateDraft(draft: CreationDraft, requireCredentials = true, requireName = true): string {
  if (requireName && !draft.name.trim()) return "请输入账号名称";
  if (!Number.isInteger(draft.concurrency) || draft.concurrency < 1) return "并发数需为正整数";
  if (!Number.isInteger(draft.priority) || draft.priority < 0) return "优先级需为非负整数";
  if (!Number.isFinite(draft.rate_multiplier) || draft.rate_multiplier < 0) return "倍率需为非负数";
  if (draft.load_factor !== null && (!Number.isInteger(draft.load_factor) || draft.load_factor < 1)) return "负载因子需为正整数";
  for (const f of fields.filter(f => fieldVisible(f,draft))) {
    const v = getField(draft,f.path);
    if (v === undefined || v === null || v === "") continue;
    if (f.kind === "number" && (typeof v !== "number" || !Number.isFinite(v) || f.min !== undefined && v < f.min || f.max !== undefined && v > f.max)) return `${f.label}超出有效范围`;
    if (f.kind === "numbers") { const a = listNumbers(v); if (!Array.isArray(a) || a.some(n => !Number.isInteger(n) || n < (f.min ?? 0) || n > (f.max ?? Infinity))) return `${f.label}需填写有效整数`; }
    if (f.kind === "url") { try { const url = new URL(String(v)); if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) return `${f.label}需为 HTTP 或 HTTPS 地址`; } catch { return `${f.label}格式无效`; } }
    if (f.kind === "mapping") {
      const entries = Object.entries(v as RecordValue);
      if (entries.length > 300 || entries.some(([a,b]) => !/^[\w.:/\-]+\*?$|^\*$/.test(a) || typeof b !== "string" || !/^[\w.:/\-]+\*?$|^\*$/.test(b) || b.includes('*') && a !== b)) return `${f.label}需使用有效模型 ID；通配白名单请将请求和转发模型填为相同值`;
    }
    if (f.kind === "headers") {
      const a = Object.entries(v as RecordValue), seen = new Set<string>();
      for (const [k,b] of a) { const key=k.toLowerCase(); if (!/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(k) || k.length>200 || catalog.blocked_headers.includes(key) || seen.has(key) || typeof b!=="string" || /[\x00-\x08\x0a-\x1f\x7f]/.test(b) || new TextEncoder().encode(b).length>8192) return "请求头名称、值或重复项无效，认证头不能覆写"; seen.add(key); }
      if(a.length>64)return "请求头最多 64 项";
    }
    if (f.kind === "pause-rules") {
      if (!Array.isArray(v) || !v.length || v.some((r: RecordValue) => !Number.isInteger(r.error_code) || Number(r.error_code)<100 || Number(r.error_code)>599 || !Number.isInteger(r.duration_minutes) || Number(r.duration_minutes)<=0 || !Array.isArray(r.keywords) || !r.keywords.length || r.keywords.some(x=>!String(x).trim()))) return "临时不可调度规则需包含状态码、关键词和正整数分钟";
    }
    if (f.kind === "protocols") {
      for (const protocol of protocols(draft.platform)) { try { const url=new URL(String((v as RecordValue)[protocol]??""));if(!["http:","https:"].includes(url.protocol) || url.username || url.password)return "协议地址需为 HTTP 或 HTTPS 地址"; }catch{return "请填写各协议的有效 API 地址";} }
    }
    if (f.kind === "protocol-rules" && (!Array.isArray(v) || v.some((r: RecordValue)=>!String(r.pattern??"").trim() || !protocols(draft.platform).includes(String(r.protocol))))) return "请填写模型匹配与有效协议";
  }
  if (requireCredentials && !isOAuth(draft)) {
    if (draft.type === "service_account") { try { buildPayload(draft); if (!draft.credentials.location) return "请输入 Vertex 区域"; } catch { return "请选择有效的 Service Account JSON 文件"; } }
    else if (draft.type === "bedrock" && draft.credentials.auth_mode === "sigv4") { if (!draft.credentials.aws_access_key_id || !draft.credentials.aws_secret_access_key) return "请填写 AWS Access Key ID 和 Secret Access Key"; }
    else if (!String(draft.credentials.api_key ?? "").trim()) return "请输入 API Key";
    if (draft.platform === "antigravity" && !draft.credentials.base_url) return "请输入上游 API 地址";
  }
  return "";
}
export function authMethods(draft: CreationDraft) {
  const methods = [{ id: "manual", label: "授权码" }];
  if (draft.platform === "anthropic") methods.push({ id: "cookie", label: "Cookie / Session Key" });
  if (["openai", "antigravity", "grok"].includes(draft.platform)) methods.push({ id: "refresh", label: "Refresh Token" });
  if (draft.platform === "openai") methods.push({ id: "mobile-refresh", label: "Mobile Refresh Token" }, { id: "codex-session", label: "Codex Session" }, { id: "agent-identity", label: "Agent Identity" }, { id: "codex-pat", label: "Codex PAT" });
  if (draft.platform === "grok") methods.push({ id: "grok-sso", label: "SSO Cookie" });
  return methods;
}
export function authProvider(draft: CreationDraft) { return draft.type === "setup-token" ? "setup-token" : draft.platform; }
export function authConfig(draft: CreationDraft) {
  const body: RecordValue = {};
  if (draft.proxy_id) body.proxy_id = draft.proxy_id;
  if (draft.platform === "gemini") for (const key of ["project_id", "oauth_type", "tier_id"]) if (draft.credentials[key]) body[key] = draft.credentials[key];
  return body;
}
export function tokenIdentity(platform: string, token: RecordValue, fallbackRefresh?: string) {
  const keys = ["access_token", "refresh_token", "id_token", "token_type", "expires_at", "scope", "email", "email_address", "org_uuid", "account_uuid", "chatgpt_account_id", "chatgpt_user_id", "organization_id", "plan_type", "subscription_expires_at", "client_id", "project_id", "oauth_type", "tier_id", "sub", "team_id", "subscription_tier", "entitlement_status"];
  const credentials = Object.fromEntries(keys.filter(k => token[k] != null && token[k] !== "").map(k => [k,token[k]]));
  if (fallbackRefresh && !credentials.refresh_token) credentials.refresh_token = fallbackRefresh;
  if (["gemini", "antigravity"].includes(platform) && credentials.expires_at !== undefined) credentials.expires_at = String(credentials.expires_at);
  const extra: RecordValue = {};
  if (platform === "gemini" && token.extra && typeof token.extra === "object") for (const key of ["drive_storage_limit","drive_storage_usage","drive_tier_updated_at"]) if (Object.prototype.hasOwnProperty.call(token.extra, key)) extra[key]=(token.extra as RecordValue)[key];
  for (const key of ["email", "email_address", "org_uuid", "account_uuid", "subscription_tier", "entitlement_status", "name", "privacy_mode"]) if (token[key]) extra[key] = token[key];
  return { credentials, extra };
}
export function importParameters(draft: CreationDraft) {
  const value = buildPayload(draft);
  const { platform: _platform, type: _type, credentials, ...rest } = value;
  return { ...rest, credential_extras: credentials };
}
export function expiryInput(value: number | null) { if (!value) return ""; const date = new Date(value*1000); return new Date(date.getTime()-date.getTimezoneOffset()*60000).toISOString().slice(0,16); }
export function parseExpiry(value: string) { return value ? Math.floor(new Date(value).getTime()/1000) : null; }
