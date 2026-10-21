import type { Account } from './types';
import { priorityPending, type PriorityAccount, type PriorityTask, type PriorityView } from './priorities';
const jobs = new Map<string, PriorityTask>();
const requests = new Map<string, { signature: string; id: string }>();
let active: PriorityTask | null = null, last: PriorityTask | null = null;
let previous = '', revision = '';
function version(accounts: Account[]) {
  const signature = JSON.stringify(accounts.map(a => [a.id, a.priority, a.settings_capabilities?.priority?.editable !== false]).sort((a,b) => Number(a[0]) - Number(b[0])));
  if (signature !== previous) { previous = signature; revision = crypto.randomUUID().replaceAll('-', '').repeat(2); }
  return revision;
}
function summary(task: PriorityTask | null) { if (!task) return null; const value = { ...task }; delete value.items; return value; }
function list(accounts: Account[]): PriorityView {
  const items: PriorityAccount[] = accounts.map(a => ({ id: a.id, name: a.name, platform: a.platform, type: a.type, priority: a.priority, version: a.operation_versions?.priority ?? a.version, editable: a.settings_capabilities?.priority?.editable !== false, reason: a.settings_capabilities?.priority?.reason ?? '', status: a.status, schedulable: a.schedulable }));
  return { items, version: version(accounts), active_task: summary(active), last_task: summary(last) };
}
export function priorityPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], emit: () => void): PriorityView | PriorityTask {
  if (method === 'GET' && path === '/account-priorities') return structuredClone(list(accounts));
  if (method === 'POST' && path === '/account-priority-orders') {
    const signature = JSON.stringify(body), old = requests.get(String(body.request_id));
    if (old) { if (signature !== old.signature) throw new Error('请求 ID 已用于其他排序'); return structuredClone(jobs.get(old.id)!); }
    if (priorityPending(active)) throw new Error('优先级排序正在执行');
    const ids = body.ordered_account_ids as number[];
    if (!Array.isArray(ids) || new Set(ids).size !== ids.length || ids.some(id => !Number.isSafeInteger(id) || id <= 0)) throw new Error('账号编号无效');
    if (body.expected_version !== version(accounts) || ids.length !== accounts.length || accounts.some(a => !ids.includes(a.id))) throw new Error('账号集合或优先级已变化，请重新核对');
    if (ids.some((id, i) => accounts.find(a => a.id === id)!.priority !== i && accounts.find(a => a.id === id)!.settings_capabilities?.priority?.editable === false)) throw new Error('排序包含不可修改的账号');
    const at = new Date().toISOString();
    const items = ids.map((id, target) => { const a = accounts.find(a => a.id === id)!; return { account_id: id, account_name: a.name, original_priority: a.priority, target_priority: target, observed_priority: a.priority === target ? target : null, status: a.priority === target ? 'unchanged' as const : 'pending' as const }; });
    const job: PriorityTask = { id: crypto.randomUUID().replaceAll('-', ''), status: 'queued', reason: '等待执行', cancel_requested: false, total: items.length, change_count: items.filter(i => i.status === 'pending').length, verified_count: items.filter(i => i.status === 'unchanged').length, created_at: at, updated_at: at, completed_at: null, items };
    jobs.set(job.id, job); requests.set(String(body.request_id), { signature, id: job.id }); active = job; last = job;
    let expected = version(accounts);
    const step = () => {
      const finish = (status: string, reason: string) => { job.status = status; job.reason = reason; job.completed_at = new Date().toISOString(); active = null; emit(); };
      if (job.cancel_requested) return finish('cancelled', '剩余修改已停止，已完成的优先级保持不变');
      if (version(accounts) !== expected) return finish('needs_confirmation', '账号集合或优先级已变化，请重新核对');
      const item = job.items!.find(i => i.status === 'pending');
      if (!item) return finish('completed', '全部优先级已回读确认');
      job.status = 'running'; job.reason = '正在保存';
      accounts.find(a => a.id === item.account_id)!.priority = item.target_priority;
      item.status = 'verified'; item.observed_priority = item.target_priority; ++job.verified_count;
      job.updated_at = new Date().toISOString(); expected = version(accounts); emit(); setTimeout(step, 350);
    };
    setTimeout(step, 800); return structuredClone(job);
  }
  const id = path.split('/')[2], job = jobs.get(id);
  if (!job) throw new Error('优先级排序任务不存在');
  if (method === 'POST' && path.endsWith('/cancel') && priorityPending(job)) { job.cancel_requested = true; job.reason = '正在停止'; }
  return structuredClone(job);
}
