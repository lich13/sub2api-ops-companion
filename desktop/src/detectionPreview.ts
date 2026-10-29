import type { Account } from './types';
import { templateBatchPreview } from './operationPreview';
import { modelTestCandidates } from './modelTestPreview';
const version = () => crypto.randomUUID().replaceAll('-', '').repeat(2);
const values = { full: { whitelist: ['gpt-6-sol', 'gpt-6-astra', 'gpt-6.1-sol'], mappings: [] }, degraded: { whitelist: ['gpt-6-luna'], mappings: [{ source: 'gpt-5.6-luna', target: 'gpt-6-luna' }, { source: 'gpt-5.6-terra', target: 'gpt-6-luna' }] }, takeover: { whitelist: ['gpt-6-luna'], mappings: [{ source: 'gpt-5.6-luna', target: 'gpt-6-luna' }, { source: 'gpt-5.6-terra', target: 'gpt-6-luna' }] } };
type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
let templates = { configured: true, version: '9'.repeat(64), templates: structuredClone(values) as Record<string, Profile>,
  template_meta: { full: { name: '满血', builtin: false }, degraded: { name: '降智', builtin: false }, takeover: { name: '降智接管', builtin: false } } as Record<string, { name: string; builtin: boolean }>,
  template_order: ['full', 'degraded', 'takeover'], template_versions: { full: 'a'.repeat(64), degraded: 'b'.repeat(64), takeover: 'c'.repeat(64) } as Record<string, string> };
const accountProfiles = new Map<number, Profile>();
const detections = new Map<number, Record<string, unknown>>();
const bank = { version: { revision: 'a'.repeat(40), sha256: 'b'.repeat(64), built_at: '2026-09-30T22:05:57Z', analyzer_version: 1 }, source: 'bundled', status: 'idle', result: 'up-to-date', checked_at: Date.now()/1000, synced_at: 0 };
export function detectionPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], model = "gpt-6-luna", platforms?: unknown) {
  if (path === '/account-templates/apply' && method === 'POST') {
    const id = String(body.template_id), profile = templates.templates[id];
    if (!profile || body.template_version !== templates.template_versions[id]) throw new Error('模板内容已变化');
    return templateBatchPreview(body, accounts, structuredClone(profile), () => templates.template_versions[id],
      aid => accountProfiles.get(aid) ?? values.full,
      (aid, value) => accountProfiles.set(aid, value));
  }
  if (path.startsWith('/account-templates')) {
    if (method !== 'GET') {
      if (body.expected_version !== templates.version) throw new Error('模板已变化');
      const id = path.split('/').at(-1)!;
      if (method === 'POST') {
        if (templates.template_order.length >= 32) throw new Error('最多保存 32 套模板');
        const created = 'custom-' + crypto.randomUUID().replaceAll('-', '').slice(0, 24);
        templates.templates[created] = { whitelist: body.whitelist as string[], mappings: body.mappings as Profile['mappings'] };
        templates.template_meta[created] = { name: String(body.name), builtin: false };
        templates.template_order.push(created); templates.template_versions[created] = version();
      } else if (method === 'DELETE') {
        delete templates.templates[id]; delete templates.template_meta[id]; delete templates.template_versions[id];
        templates.template_order = templates.template_order.filter(item => item !== id);
      } else if (path === '/account-templates') {
        for (const key of ['full', 'degraded', 'takeover']) if (!templates.templates[key]) throw new Error('模板已删除，请升级客户端');
        for (const key of ['full', 'degraded', 'takeover']) { templates.templates[key] = body[key] as Profile; templates.template_versions[key] = version(); }
      } else {
        if (!templates.templates[id]) throw new Error('模板不存在');
        const profile = { whitelist: body.whitelist as string[], mappings: body.mappings as Profile['mappings'] };
        if (JSON.stringify(profile) !== JSON.stringify(templates.templates[id])) templates.template_versions[id] = version();
        templates.templates[id] = profile; templates.template_meta[id] = { name: String(body.name), builtin: false };
      }
      templates = { ...templates, configured: true, version: version() };
    }
    const aid = Number(new URLSearchParams(path.split('?')[1]).get('account_id'));
    const account = accounts.find(a => a.id === aid);
    const eligible = !!account && account.platform === 'openai' && ['oauth', 'apikey'].includes(account.type) && !account.parent_account_id;
    return structuredClone({ ...templates, ...(account ? { account: { id: aid, version: account.version, eligible, passthrough: false, reason: eligible ? '' : '仅支持独立 OpenAI OAuth 或 Key 账号', config: accountProfiles.get(aid) ?? values.full } } : {}) });
  }
  if (path.startsWith('/modeltrace/fingerprint-bank') || path.startsWith('/lm-detector/fingerprint-bank')) {
    if (method === 'POST') { bank.status = 'checking'; setTimeout(() => { bank.status = 'idle'; bank.checked_at = Date.now()/1000; }, 800); }
    return structuredClone(path.startsWith("/lm-detector/") ? { ...bank, version: { ...bank.version, analyzer_version: "lmfpd-0.2.0" } } : bank);
  }
  const aid = Number(path.split('/')[2]);
  let value = detections.get(aid) || { account_id: aid, version: 'd'.repeat(64), enabled: false, interval_minutes: 15, schedule_mode: 'smart', min_interval_minutes: 5, recent_request_count: aid === 102 ? 0 : 60, effective_interval_minutes: aid === 102 ? null : 10, schedule_anchor_at: null, model_id: 'gpt-6-luna', status: 'disabled', next_at: null,
    last_automatic_started_at: aid === 102 ? '2026-10-01T02:30:00Z' : null,
    last_result: aid === 102 ? { status: 'completed', report: { prediction_name: '预览模型' }, completed_at: '2026-10-01T02:30:12Z' } : null };
  const account = accounts.find(a => a.id === aid);
  const openai = account?.platform === "openai";
  const selection = (platforms as Record<string, { engine: string; sample_count: number }>)?.[account?.platform ?? "openai"];
  const defaultModel = openai ? model : "";
  value = { ...value, engine: selection?.engine ?? (openai ? "modeltrace" : "lm_detector"), sample_count: selection?.engine === "lm_detector" ? selection.sample_count : openai ? 3 : 1 };
  const effectiveModel = value.model_override ?? defaultModel;
  if (value.model_id !== effectiveModel) value = { ...value, version: version() };
  value = { ...value, model_id: effectiveModel, model_source: value.model_override || !openai ? "account" : "global", model_override: value.model_override ?? null, global_model_id: defaultModel };
  detections.set(aid, value);
  if (method === 'PUT') {
    if (body.expected_version !== value.version) throw new Error('检测设置已变化');
    const account = accounts.find((a) => a.id === aid);
    if (body.model_override !== undefined && account?.type !== "apikey") throw new Error('仅 API Key 账号可单独指定检测模型');
    const requested = body.model_override !== undefined ? body.model_override : value.model_override;
    const override = typeof requested === "string" ? requested.trim() : requested;
    if (override !== null && (typeof override !== "string" || !/^[A-Za-z0-9._:/-]{1,200}$/.test(override))) throw new Error('检测模型无效');
    const selected = override ?? defaultModel;
    if (body.enabled && (!account || !modelTestCandidates(account).some((m) => m.id === selected))) throw new Error('所选模型不在当前账号上游模型目录中');
    const schedule = body.schedule_mode ?? value.schedule_mode;
    const minimum = body.min_interval_minutes ?? value.min_interval_minutes;
    if (!['fixed', 'smart'].includes(String(schedule))) throw new Error('检测模式无效');
    if (!Number.isInteger(minimum) || Number(minimum) < 5 || Number(minimum) > 30) throw new Error('最小间隔须为 5–30 的整数分钟');
    const count = Number(value.recent_request_count);
    const tier = count >= 120 ? 5 : count >= 60 ? 10 : count >= 30 ? 15 : count >= 10 ? 20 : count >= 3 ? 25 : count > 0 ? 30 : null;
    const effective = schedule === 'smart' ? tier === null ? null : Math.max(tier, Number(minimum)) : Number(body.interval_minutes);
    const anchor = value.schedule_anchor_at ?? new Date().toISOString();
    value = { ...value, schedule_mode: schedule, min_interval_minutes: minimum, effective_interval_minutes: effective, schedule_anchor_at: anchor, model_override: override, model_id: selected, model_source: override || !openai ? "account" : "global", enabled: body.enabled, interval_minutes: body.interval_minutes, version: version(), status: body.enabled ? effective === null ? 'paused' : 'waiting' : 'disabled', reason: body.enabled && effective === null ? '最近一小时无业务调用，已暂停定时检测' : '', next_at: body.enabled && effective !== null ? new Date((schedule === 'smart' ? Date.parse(String(anchor)) : Date.now()) + effective*60000).toISOString() : null };
    detections.set(aid, value);
  }
  return structuredClone(value);
}
