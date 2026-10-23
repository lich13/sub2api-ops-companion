import type { Account, Recovery } from './types';
import type { Operation } from './accountOperations';
import { applySettingsPreview, isSettingsAction, previewProxies, settingsPreview } from './settingsPreview';
function recoverPreview(account: Account) {
  const wasError = account.status === 'error';
  if (wasError) account.status = 'active';
  account.blockers = account.blockers.filter(blocker => !(wasError && blocker.code === 'status') && !['rate_limit_reset_at', 'overload_until', 'temp_unschedulable_until'].includes(blocker.code));
  account.recoverable = false; account.error_message = '';
  account.available = account.schedulable && account.status === 'active' && account.blockers.length === 0;
  if (account.operation_versions) account.operation_versions.recover = crypto.randomUUID().replaceAll('-', '').repeat(2);
}
const jobs = new Map<string, Operation>();
const scheduledHistory = new Map<string, Recovery>();
export const scheduledTestHistory = () => [...scheduledHistory.values()].reverse();
function recordScheduled(job: Operation) {
  if (!job.scheduled_at || scheduledHistory.has(job.id)) return;
  scheduledHistory.set(job.id, { id: 10000 + scheduledHistory.size, account_id: job.account_id, account_name: job.account_name, model_id: String(job.requested.model_id || ""), test_completed_at: new Date().toISOString(), recovered_at: null, legacy: false, kind: 'connection_test', result_status: job.status });
}
const requests = new Map<string, string>();
const batches = new Map<string, string[]>();
const batchRequests = new Map<string, { id: string; signature: string }>();
export async function operationPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], emit: () => void) {
  if (method === 'POST' && path === '/account-operations/batch') {
    if (!['proxy', 'delete', 'groups', 'test', 'recover'].includes(String(body.action))) throw new Error('批量操作类型无效');
    if (body.action === 'recover' && (!body.payload || typeof body.payload !== 'object' || Object.keys(body.payload).length)) throw new Error('恢复参数无效');
    if (body.action === 'delete' && (body.payload as Record<string, unknown>)?.confirmed !== true) throw new Error('请先确认删除');
    if (body.action === 'test' && (body.payload as Record<string, unknown>)?.confirmed !== true) throw new Error('请先确认连接测试');
    const signature = JSON.stringify(body), old = batchRequests.get(String(body.request_id));
    if (old && old.signature !== signature) throw new Error('请求 ID 已用于其他操作');
    const bid = old?.id ?? crypto.randomUUID().replaceAll('-', '');
    if (!old) {
      const targets = body.accounts as { account_id: number; expected_version: string; payload?: Record<string, unknown> }[];
      if (!targets?.length || targets.length > 1000 || new Set(targets.map(t => t.account_id)).size !== targets.length) throw new Error('批量账号无效');
      const ids = targets.map(target => {
        const account = accounts.find(a => a.id === target.account_id);
        const action = String(body.action), payload = (action === 'test'
          ? { confirmed: true, mode: account?.platform === 'grok' ? 'text' : 'default', model_id: account?.platform === 'grok' ? '' : 'gpt-6-luna' }
          : ['proxy', 'recover'].includes(action) ? body.payload : target.payload) as Record<string, unknown>;
        const available = action !== 'proxy' || payload.proxy_id === null || previewProxies.some(p => p.id === payload.proxy_id && p.status === 'active');
        const settings = account ? settingsPreview(account) : null;
        const eligible = action === 'proxy' ? !!settings?.capabilities.proxy.editable : action === 'test'
          ? !!account && ['openai', 'grok'].includes(account.platform) && ['oauth', 'apikey'].includes(account.type) : !!account;
        const conflict = (action === 'proxy' ? settings?.versions.proxy : account?.operation_versions?.[action] ?? account?.version) !== target.expected_version;
        const unchanged = action === 'proxy' && eligible && settings?.proxy_id === payload.proxy_id || action === 'recover' && eligible && !conflict && !account?.recoverable;
        const job: Operation = { id: crypto.randomUUID().replaceAll('-', ''), batch_id: bid, account_id: target.account_id,
          account_name: account?.name ?? '账号', action, requested: payload, ...(['proxy', 'delete', 'groups'].includes(action) ? { execution_mode: 'native' as const } : {}),
          status: !eligible ? 'failed' : unchanged ? 'completed' : conflict || !available ? 'needs_confirmation' : 'queued',
          reason: !eligible ? settings?.capabilities.proxy.reason ?? '账号不存在' : unchanged ? '无变化' : !available ? '所选代理不可用' : conflict ? '账号相关状态已变化' : '等待执行',
          ...(unchanged ? { result: { verified: true, unchanged: true } } : {}) };
        jobs.set(job.id, job);
        if (job.status === 'queued') setTimeout(() => {
          if (job.status !== 'queued' || !account) return;
          try {
            if (action === 'test') job.result = { requested_model: payload.model_id,
              actual_model: account.platform === 'grok' ? 'grok' : 'gpt-6-luna', success: account.status !== 'error',
              duration_ms: 360, completed_at: new Date().toISOString(), ...(account.status === 'error' ? { error: '测试请求失败 [http_401]' } : {}) };
            else if (action === 'recover') { recoverPreview(account); job.result = { verified: true, account_id: account.id }; }
            else if (action === 'proxy') job.result = applySettingsPreview(account, 'proxy', payload, target.expected_version);
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
  if (method === 'POST' && /^\/account-operations\/batches\/[a-f0-9]{32}\/cancel$/.test(path)) {
    const bid = path.split('/')[3], ids = batches.get(bid);
    if (!ids || ids.some(id => jobs.get(id)?.action !== 'test')) throw new Error('批量测试不存在');
    for (const id of ids) {
      const job = jobs.get(id)!;
      if (['queued', 'needs_confirmation'].includes(job.status) && !job.started_at) { job.status = 'cancelled'; job.reason = '已停止，未派发测试'; }
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
    const at = typeof body.scheduled_at === 'string' ? Date.parse(body.scheduled_at) : undefined;
    if (at !== undefined && (action !== 'test' || !Number.isFinite(at) || at <= Date.now() || at % 60000 !== 0)) throw new Error('预约时间无效');
    const job: Operation = { id: crypto.randomUUID().replaceAll('-', ''), account_id: account.id, account_name: account.name, action, requested: payload, status: 'queued', reason: at ? '等待预约时间' : '等待账号空闲', ...(at ? { scheduled_at: new Date(at).toISOString() } : {}) };
    jobs.set(job.id, job); requests.set(String(body.request_id), job.id);
    const finish = () => {
      if (job.status !== 'queued') return;
      if (at && at > Date.now()) { setTimeout(finish, Math.min(at - Date.now(), 2_147_483_647)); return; }
      let settingsResult: Record<string, unknown> = {};
      if (isSettingsAction(action)) {
        try { settingsResult = applySettingsPreview(account, action, payload, String(body.expected_version)); }
        catch (e) { job.status = 'needs_confirmation'; job.reason = String(e).replace(/^Error: /, ''); emit(); return; }
      }
      if (action === 'schedulable') {
        account.schedulable = Boolean(payload.schedulable);
        if (payload.detach_managed) account.managed = false;
        account.blockers = account.blockers.filter(blocker => blocker.code !== 'disabled');
        if (!account.schedulable) account.blockers.push({ code: 'disabled', label: '调度已关闭' });
        account.available = account.status === 'active' && account.blockers.length === 0;
      }
      if (action === 'recover') recoverPreview(account);
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
      recordScheduled(job); emit();
    };
    setTimeout(finish, at ? Math.min(at - Date.now(), 2_147_483_647) : 1200);
    return structuredClone(job);
  }
  const job = jobs.get(path.split('/')[2]?.split('?')[0]);
  if (!job) throw new Error('操作不存在');
  if (method === 'POST' && path.endsWith('/cancel') && job.status === 'queued') { job.status = 'cancelled'; recordScheduled(job); }
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
