import type { Account } from './types';
const version = () => crypto.randomUUID().replaceAll('-', '').repeat(2);
const values = { full: { whitelist: ['gpt-6-sol', 'gpt-6-astra', 'gpt-6.1-sol'], mappings: [] }, degraded: { whitelist: ['gpt-6-luna'], mappings: [{ source: 'gpt-5.6-luna', target: 'gpt-6-luna' }, { source: 'gpt-5.6-terra', target: 'gpt-6-luna' }] }, takeover: { whitelist: ['gpt-6-luna'], mappings: [{ source: 'gpt-5.6-luna', target: 'gpt-6-luna' }, { source: 'gpt-5.6-terra', target: 'gpt-6-luna' }] } };
type Custom = { id: string; name: string; whitelist: string[]; mappings: { source: string; target: string }[] };
let templates = { custom_templates: [] as Custom[], configured: true, version: '9'.repeat(64), templates: values };
const detections = new Map<number, Record<string, unknown>>();
const bank = { version: { revision: 'a'.repeat(40), sha256: 'b'.repeat(64), built_at: '2026-09-30T22:05:57Z', analyzer_version: 1 }, source: 'bundled', status: 'idle', result: 'up-to-date', checked_at: Date.now()/1000, synced_at: 0 };
export function detectionPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[]) {
  if (path.startsWith('/account-templates')) {
    if (method !== 'GET') {
      if (body.expected_version !== templates.version) throw new Error('模板已变化');
      const id = path.split('/').at(-1)!;
      if (method === 'POST') {
        if (templates.custom_templates.length >= 29) throw new Error('最多保存 32 套模板');
        templates.custom_templates.push({ id: 'custom-' + crypto.randomUUID().replaceAll('-', '').slice(0, 24), name: String(body.name), whitelist: body.whitelist as string[], mappings: body.mappings as Custom['mappings'] });
      } else if (method === 'DELETE') {
        templates.custom_templates = templates.custom_templates.filter(item => item.id !== id);
      } else if (path === '/account-templates') {
        templates.templates = { full: body.full, degraded: body.degraded, takeover: body.takeover } as typeof values;
      } else {
        templates.custom_templates = templates.custom_templates.map(item => item.id === id ? { ...item, name: String(body.name), whitelist: body.whitelist as string[], mappings: body.mappings as Custom['mappings'] } : item);
      }
      templates = { ...templates, configured: true, version: version() };
    }
    const aid = Number(new URLSearchParams(path.split('?')[1]).get('account_id'));
    const account = accounts.find(a => a.id === aid);
    return structuredClone({ ...templates, ...(account ? { account: { id: aid, version: account.version, eligible: true, passthrough: false, config: values.full } } : {}) });
  }
  if (path.startsWith('/modeltrace/fingerprint-bank')) {
    if (method === 'POST') { bank.status = 'checking'; setTimeout(() => { bank.status = 'idle'; bank.checked_at = Date.now()/1000; }, 800); }
    return structuredClone(bank);
  }
  const aid = Number(path.split('/')[2]);
  let value = detections.get(aid) || { account_id: aid, version: 'd'.repeat(64), enabled: false, interval_minutes: 15, model_id: 'gpt-6-luna', status: 'disabled', next_at: null };
  if (method === 'PUT') {
    if (body.expected_version !== value.version) throw new Error('检测设置已变化');
    value = { ...value, ...body, version: version(), status: body.enabled ? 'waiting' : 'disabled', next_at: body.enabled ? new Date(Date.now() + Number(body.interval_minutes)*60000).toISOString() : null };
    detections.set(aid, value);
  }
  return structuredClone(value);
}
