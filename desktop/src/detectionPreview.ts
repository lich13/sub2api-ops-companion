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
export function detectionPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], model = "gpt-6-luna") {
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
  if (path.startsWith('/modeltrace/fingerprint-bank')) {
    if (method === 'POST') { bank.status = 'checking'; setTimeout(() => { bank.status = 'idle'; bank.checked_at = Date.now()/1000; }, 800); }
    return structuredClone(bank);
  }
  const aid = Number(path.split('/')[2]);
  let value = detections.get(aid) || { account_id: aid, version: 'd'.repeat(64), enabled: false, interval_minutes: 15, model_id: 'gpt-6-luna', status: 'disabled', next_at: null,
    last_automatic_started_at: aid === 102 ? '2026-10-01T02:30:00Z' : null,
    last_result: aid === 102 ? { status: 'completed', report: { prediction_name: '预览模型' }, completed_at: '2026-10-01T02:30:12Z' } : null };
  const effectiveModel = value.model_override ?? model;
  if (value.model_id !== effectiveModel) value = { ...value, version: version() };
  value = { ...value, model_id: effectiveModel, model_source: value.model_override ? "account" : "global", model_override: value.model_override ?? null, global_model_id: model };
  detections.set(aid, value);
  if (method === 'PUT') {
    if (body.expected_version !== value.version) throw new Error('检测设置已变化');
    const account = accounts.find((a) => a.id === aid);
    if (body.model_override !== undefined && (account?.platform !== "openai" || account.type !== "apikey")) throw new Error('仅 OpenAI Key 账号可单独指定检测模型');
    const requested = body.model_override !== undefined ? body.model_override : value.model_override;
    const override = typeof requested === "string" ? requested.trim() : requested;
    if (override !== null && (typeof override !== "string" || !/^[A-Za-z0-9._:/-]{1,200}$/.test(override))) throw new Error('检测模型无效');
    const selected = override ?? model;
    if (body.enabled && (!account || !modelTestCandidates(account).some((m) => m.id === selected))) throw new Error('所选模型不在当前账号上游模型目录中');
    value = { ...value, model_override: override, model_id: selected, model_source: override ? "account" : "global", enabled: body.enabled, interval_minutes: body.interval_minutes, version: version(), status: body.enabled ? 'waiting' : 'disabled', next_at: body.enabled ? new Date(Date.now() + Number(body.interval_minutes)*60000).toISOString() : null };
    detections.set(aid, value);
  }
  return structuredClone(value);
}
