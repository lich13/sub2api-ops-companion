import type { Account } from './types';
import { groupPriorityAccounts, priorityPending, samePriorityAccounts, type PriorityAccount, type PriorityTask, type PriorityView } from './priorities';
const jobs = new Map<string, PriorityTask>();
const requests = new Map<string, { signature: string; id: string }>();
let active: PriorityTask | null = null;
const revisions = new Map<string, { signature: string; version: string }>();
const widths = new Map<string, number>();
function version(accounts: Account[], platform: string) {
  const width = widths.get(platform) ?? 1;
  const signature = JSON.stringify([width, accounts.filter(a => a.platform === platform).map(a => [a.id, a.priority, a.settings_capabilities?.priority?.editable !== false]).sort((a,b) => Number(a[0]) - Number(b[0]))]);
  if (signature !== revisions.get(platform)?.signature) revisions.set(platform, { signature, version: crypto.randomUUID().replaceAll('-', '').repeat(2) });
  return revisions.get(platform)!.version;
}
function summary(task: PriorityTask | null) { if (!task) return null; const value = { ...task }; delete value.items; return value; }
function list(accounts: Account[]): PriorityView {
  const items: PriorityAccount[] = accounts.map(a => ({ id: a.id, name: a.name, platform: a.platform, type: a.type, priority: a.priority, version: a.operation_versions?.priority ?? a.version, editable: a.settings_capabilities?.priority?.editable !== false, reason: a.settings_capabilities?.priority?.reason ?? '', status: a.status, schedulable: a.schedulable }));
  return { platforms: groupPriorityAccounts(items).map(group => ({ ...group, parallelism: widths.get(group.platform) ?? 1, version: version(accounts, group.platform), last_task: summary([...jobs.values()].reverse().find(task => task.platform === group.platform) ?? null) })), active_task: summary(active), legacy_last_task: null };
}
export function priorityPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], emit: () => void): PriorityView | PriorityTask {
  const [plain, query] = path.split('?'), params = new URLSearchParams(query);
  if (params.get('schema') !== '2') throw new Error('请升级客户端以按平台排序');
  if (method === 'GET' && plain === '/account-priorities') return structuredClone(list(accounts));
  if (method === 'PUT' && plain === '/account-priorities') {
    const platform = String(body.platform), next = Number(body.parallelism);
    if (!Number.isInteger(next) || next < 1 || next > 8 || body.expected_version !== version(accounts, platform)) throw new Error('账号集合或优先级已变化，请重新核对');
    widths.set(platform, next); emit();
    return structuredClone(list(accounts));
  }
  if (method === 'POST' && plain === '/account-priority-orders') {
    const signature = JSON.stringify(body), old = requests.get(String(body.request_id));
    if (old) { if (signature !== old.signature) throw new Error('请求 ID 已用于其他排序'); return structuredClone(jobs.get(old.id)!); }
    if (priorityPending(active)) throw new Error('优先级排序正在执行');
    const ids = body.ordered_account_ids as number[], platform = String(body.platform), selected = accounts.filter(a => a.platform === platform);
    if (!Array.isArray(ids) || new Set(ids).size !== ids.length || ids.some(id => !Number.isSafeInteger(id) || id <= 0)) throw new Error('账号编号无效');
    const parallelism = Number(body.parallelism ?? widths.get(platform) ?? 1);
    if (parallelism !== (widths.get(platform) ?? 1) || !selected.length || !Number.isInteger(parallelism) || parallelism < 1 || parallelism > 8 || body.expected_version !== version(accounts, platform) || !samePriorityAccounts(ids, selected as unknown as PriorityAccount[])) throw new Error('账号集合或优先级已变化，请重新核对');
    const byId = new Map(selected.map(a => [a.id, a]));
    const targets = (body.target_priorities ?? ids.map((_, i) => Math.floor(i / parallelism) + 1)) as number[];
    if (!Array.isArray(targets) || targets.length !== ids.length || targets.some(n => !Number.isInteger(n) || n < 0 || n > 2147483647 || targets.filter(t => t === n).length > parallelism)) throw new Error('并列槽位数量超过平台设置');
    if (ids.some((id, i) => byId.get(id)!.priority !== targets[i] && byId.get(id)!.settings_capabilities?.priority?.editable === false)) throw new Error('排序包含不可修改的账号');
    const at = new Date().toISOString();
    const items = ids.map((id, index) => { const a = byId.get(id)!, target = targets[index]; return { account_id: id, account_name: a.name, platform, original_priority: a.priority, target_priority: target, observed_priority: a.priority === target ? target : null, status: a.priority === target ? 'unchanged' as const : 'pending' as const }; });
    const job: PriorityTask = { id: crypto.randomUUID().replaceAll('-', ''), platform, parallelism, base: 1, waiting_reason: '', waiting_account_ids: [], status: 'queued', reason: '等待执行', cancel_requested: false, total: items.length, change_count: items.filter(i => i.status === 'pending').length, verified_count: items.filter(i => i.status === 'unchanged').length, created_at: at, updated_at: at, completed_at: null, items };
    jobs.set(job.id, job); requests.set(String(body.request_id), { signature, id: job.id }); active = job;
    let expected = version(accounts, platform);
    const step = () => {
      const finish = (status: string, reason: string) => { job.status = status; job.reason = reason; job.updated_at = job.completed_at = new Date().toISOString(); active = null; emit(); };
      if (job.cancel_requested) return finish('cancelled', '剩余修改已停止，已完成的优先级保持不变');
      if (version(accounts, platform) !== expected) return finish('needs_confirmation', '账号集合或优先级已变化，请重新核对');
      const item = job.items!.find(i => i.status === 'pending');
      if (!item) return finish('completed', '该平台全部优先级已回读确认');
      job.status = 'running'; job.reason = '正在保存';
      byId.get(item.account_id)!.priority = item.target_priority;
      item.status = 'verified'; item.observed_priority = item.target_priority; ++job.verified_count;
      job.updated_at = new Date().toISOString(); expected = version(accounts, platform); emit(); setTimeout(step, 350);
    };
    setTimeout(step, 0); return structuredClone(job);
  }
  const id = plain.split('/')[2], job = jobs.get(id);
  if (!job) throw new Error('优先级排序任务不存在');
  if (method === 'POST' && plain.endsWith('/cancel') && priorityPending(job)) { job.cancel_requested = true; job.reason = '正在停止'; }
  return structuredClone(params.get('details') === '0' ? summary(job)! : job);
}
