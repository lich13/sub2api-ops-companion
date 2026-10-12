import type { Account } from './types';
import type { Operation } from './accountOperations';
import { applySettingsPreview, isSettingsAction, previewProxies, settingsPreview } from './settingsPreview';
const jobs = new Map<string, Operation>();
const requests = new Map<string, string>();
const batches = new Map<string, string[]>();
const batchRequests = new Map<string, { id: string; signature: string }>();
export async function operationPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], emit: () => void) {
  if (method === 'POST' && path === '/account-operations/batch') {
    if (!['proxy', 'delete', 'groups'].includes(String(body.action))) throw new Error('批量操作类型无效');
    if (body.action === 'delete' && (body.payload as Record<string, unknown>)?.confirmed !== true) throw new Error('请先确认删除');
    const signature = JSON.stringify(body), old = batchRequests.get(String(body.request_id));
    if (old && old.signature !== signature) throw new Error('请求 ID 已用于其他操作');
    const bid = old?.id ?? crypto.randomUUID().replaceAll('-', '');
    if (!old) {
      const targets = body.accounts as { account_id: number; expected_version: string; payload?: Record<string, unknown> }[];
      if (!targets?.length || targets.length > 1000 || new Set(targets.map(t => t.account_id)).size !== targets.length) throw new Error('批量账号无效');
      const ids = targets.map(target => {
        const action = String(body.action), payload = (action === 'proxy' ? body.payload : target.payload) as Record<string, unknown>;
        const available = action !== 'proxy' || payload.proxy_id === null || previewProxies.some(p => p.id === payload.proxy_id && p.status === 'active');
        const account = accounts.find(a => a.id === target.account_id), settings = account ? settingsPreview(account) : null;
        const eligible = action === 'proxy' ? !!settings?.capabilities.proxy.editable : !!account;
        const unchanged = action === 'proxy' && eligible && settings?.proxy_id === payload.proxy_id;
        const conflict = (action === 'proxy' ? settings?.versions.proxy : account?.operation_versions?.[action] ?? account?.version) !== target.expected_version;
        const job: Operation = { id: crypto.randomUUID().replaceAll('-', ''), batch_id: bid, account_id: target.account_id,
          account_name: account?.name ?? '账号', action, requested: payload, execution_mode: 'native',
          status: !eligible ? 'failed' : unchanged ? 'completed' : conflict || !available ? 'needs_confirmation' : 'queued',
          reason: !eligible ? settings?.capabilities.proxy.reason ?? '账号不存在' : unchanged ? '无变化' : !available ? '所选代理不可用' : conflict ? '代理配置已变化' : '等待执行',
          ...(unchanged ? { result: { verified: true, unchanged: true } } : {}) };
        jobs.set(job.id, job);
        if (job.status === 'queued') setTimeout(() => {
          if (job.status !== 'queued' || !account) return;
          try {
            if (action === 'proxy') job.result = applySettingsPreview(account, 'proxy', payload, target.expected_version);
            else if (action === 'groups') {
              const scope = payload.scope_group_ids as number[], targetGroups = payload.group_ids as number[];
              account.group_ids = [...new Set([...account.group_ids.filter(id => !scope.includes(id)), ...targetGroups])].sort((a, b) => a - b);
              job.result = { verified: true, group_ids: account.group_ids };
            } else {
              for (let i = accounts.length - 1; i >= 0; --i) if (accounts[i].id === account.id || accounts[i].parent_account_id === account.id) accounts.splice(i, 1);
              job.result = { verified: true, deleted: true, detached: account.managed };
            }
            job.status = 'completed'; job.reason = '';
          }
          catch (e) { job.status = 'needs_confirmation'; job.reason = String(e).replace(/^Error: /, ''); }
          emit();
        }, 1200);
        return job.id;
      });
      batches.set(bid, ids); batchRequests.set(String(body.request_id), { id: bid, signature });
    }
    return operationPreview('GET', `/account-operations?batch_id=${bid}`, {}, accounts, emit);
  }
  if (path.split('?')[0] === '/account-operations') {
    const bid = new URLSearchParams(path.split('?')[1]).get('batch_id');
    if (bid && !batches.has(bid)) throw new Error('批次不存在');
    const items = bid ? batches.get(bid)!.map(id => jobs.get(id)!) : [...jobs.values()].reverse();
    return { ...(bid ? { batch_id: bid } : {}), items: structuredClone(items), pending: items.filter((j) => ['queued', 'running', 'checking', 'needs_confirmation'].includes(j.status)).length };
  }
  if (method === 'POST' && path.endsWith('/operations')) {
    const old = requests.get(String(body.request_id));
    if (old) return structuredClone(jobs.get(old));
    const account = accounts.find((a) => a.id === Number(path.split('/')[2]));
    if (!account) throw new Error('账号不存在');
    const action = String(body.action), payload = body.payload as Record<string, unknown>;
    const job: Operation = { id: crypto.randomUUID().replaceAll('-', ''), account_id: account.id, account_name: account.name, action, requested: payload, status: 'queued', reason: '等待账号空闲' };
    jobs.set(job.id, job); requests.set(String(body.request_id), job.id);
    setTimeout(() => {
      if (job.status !== 'queued') return;
      let settingsResult: Record<string, unknown> = {};
      if (isSettingsAction(action)) {
        try { settingsResult = applySettingsPreview(account, action, payload, String(body.expected_version)); }
        catch (e) { job.status = 'needs_confirmation'; job.reason = String(e).replace(/^Error: /, ''); emit(); return; }
      }
      if (action === 'schedulable') account.schedulable = Boolean(payload.schedulable);
      if (action === 'groups') {
        const scope = payload.scope_group_ids as number[], target = payload.group_ids as number[];
        account.group_ids = [...new Set([...account.group_ids.filter((id) => !scope.includes(id)), ...target])];
      }
      if (action === 'degradation_mark') account.degradation_mark = { marked: Boolean(payload.marked), marked_at: new Date().toISOString(), version: crypto.randomUUID().replaceAll('-', '').repeat(2) };
      if (action === 'test') {
        job.events = [{ type: 'test_complete', success: true, duration_ms: 500, completed_at: new Date().toISOString() }];
        job.next_event = 1;
      }
      job.status = 'completed'; job.reason = '';
      job.result = { verified: true, priority: account.priority, group_ids: account.group_ids, version: account.version, degradation_mark: account.degradation_mark, message: '操作已完成', deleted: action === 'delete', success: true, ...settingsResult };
      emit();
    }, 1200);
    return structuredClone(job);
  }
  const job = jobs.get(path.split('/')[2]?.split('?')[0]);
  if (!job) throw new Error('操作不存在');
  if (method === 'POST' && path.endsWith('/cancel') && job.status === 'queued') job.status = 'cancelled';
  return structuredClone(job);
}


type TemplateProfile = { whitelist: string[]; mappings: { source: string; target: string }[] };
export function templateBatchPreview(body: Record<string, unknown>, accounts: Account[], profile: TemplateProfile,
  currentVersion: () => string | undefined, read: (id: number) => TemplateProfile, write: (id: number, value: TemplateProfile) => void) {
  const signature = JSON.stringify(body), old = batchRequests.get(String(body.request_id));
  if (old && old.signature !== signature) throw new Error('请求 ID 已用于其他操作');
  const bid = old?.id ?? crypto.randomUUID().replaceAll('-', '');
  if (!old) {
    const items = (body.accounts as { account_id: number; expected_version: string }[]).map(target => {
      const account = accounts.find(a => a.id === target.account_id);
      const eligible = account?.platform === 'openai' && ['oauth', 'apikey'].includes(account.type) && !account.parent_account_id;
      const unchanged = JSON.stringify(read(target.account_id)) === JSON.stringify(profile);
      const conflict = account?.version !== target.expected_version;
      const job: Operation = { id: crypto.randomUUID().replaceAll('-', ''), batch_id: bid, account_id: target.account_id, account_name: account?.name ?? '账号', action: 'account_template',
        requested: { template_id: body.template_id, template_version: body.template_version },
        status: !eligible ? 'failed' : unchanged ? 'completed' : conflict ? 'needs_confirmation' : 'queued',
        reason: !eligible ? '仅支持独立 OpenAI OAuth 或 Key 账号' : unchanged ? '配置无变化' : conflict ? '账号配置已变化' : '等待账号空闲' };
      jobs.set(job.id, job);
      if (job.status === 'queued') setTimeout(() => {
        if (job.status !== 'queued') return;
        if (currentVersion() !== body.template_version || account?.version !== target.expected_version) { job.status = 'needs_confirmation'; job.reason = '模板或账号配置已变化'; return; }
        write(target.account_id, structuredClone(profile)); job.status = 'completed'; job.reason = ''; job.result = { verified: true };
      }, 1200);
      return job.id;
    });
    batches.set(bid, items); batchRequests.set(String(body.request_id), { id: bid, signature });
  }
  const items = batches.get(bid)!.map(id => jobs.get(id)!);
  return structuredClone({ batch_id: bid, items, pending: items.filter(j => ['queued', 'running', 'checking', 'needs_confirmation'].includes(j.status)).length });
}
