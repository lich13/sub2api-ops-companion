import type { Account } from './types';
import type { AccountSettings } from './AccountSettingsDialog';
import type { Profile } from './AccountTemplates';

const configs = new Map<number, { model: Profile; proxy: number | null }>();
const fields = ['priority', 'concurrency', 'load_factor', 'model_config', 'proxy', 'sync_models'];
export const previewProxies = [
  { id: 1, name: '演示代理', protocol: 'http', status: 'active' },
  { id: 2, name: '备用代理', protocol: 'socks5', status: 'inactive' },
];
function config(account: Account) {
  if (!configs.has(account.id)) configs.set(account.id, { model: { whitelist: ['fixture-text'], mappings: [] }, proxy: null });
  return configs.get(account.id)!;
}
export function settingsPreview(account: Account): AccountSettings {
  const value = config(account), inherited = !!account.parent_account_id;
  account.operation_versions ??= {};
  for (const field of fields) account.operation_versions[field] ??= account.version;
  return structuredClone({ account_id: account.id, priority: account.priority, concurrency: account.concurrency ?? 2,
    load_factor: account.load_factor ?? null, effective_load_factor: account.load_factor ?? account.concurrency ?? 2,
    proxy_id: value.proxy, model_config: value.model,
    capabilities: Object.fromEntries(fields.map(field => [field, {
      editable: !inherited || ['priority', 'concurrency', 'load_factor'].includes(field),
      reason: inherited && !['priority', 'concurrency', 'load_factor'].includes(field) ? '继承母账号设置' : '',
    }])), versions: Object.fromEntries(fields.map(field => [field, account.operation_versions![field]])) });
}
export function applySettingsPreview(account: Account, action: string, payload: Record<string, unknown>, expected: string) {
  const settings = settingsPreview(account), value = config(account);
  if (settings.versions[action] !== expected) throw new Error('相关字段已变化，请重新读取');
  if (!settings.capabilities[action]?.editable) throw new Error('继承母账号设置，不能独立修改');
  if (action === 'sync_models') return { models: ['fixture-text', 'fixture-new-text', 'fixture-reasoning'], warnings: [], synced_at: new Date().toISOString() };
  if (action === 'model_config') value.model = structuredClone({ whitelist: payload.whitelist as string[], mappings: payload.mappings as Profile['mappings'] });
  else if (action === 'proxy') value.proxy = payload.proxy_id as number | null;
  else if (action === 'priority' || action === 'concurrency') account[action] = Number(payload[action]);
  else if (action === 'load_factor') account.load_factor = payload.load_factor as number | null;
  const version = crypto.randomUUID().replaceAll('-', '').repeat(2);
  account.operation_versions![action] = version;
  return { verified: true, ...payload, field_version: version, settings_versions: settingsPreview(account).versions };
}
export const isSettingsAction = (action: string) => fields.includes(action);
