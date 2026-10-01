import type { Account } from './types';
import type { Operation } from './accountOperations';
const jobs = new Map<string, Operation>();
const requests = new Map<string, string>();
export async function operationPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[], emit: () => void) {
  if (path === '/account-operations') {
    const items = [...jobs.values()].reverse();
    return { items, pending: items.filter((j) => ['queued', 'running', 'checking', 'needs_confirmation'].includes(j.status)).length };
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
      if (action === 'priority') account.priority = Number(payload.priority);
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
      job.result = { verified: true, priority: account.priority, group_ids: account.group_ids, version: account.version, degradation_mark: account.degradation_mark, message: '操作已完成', deleted: action === 'delete', success: true };
      emit();
    }, 1200);
    return structuredClone(job);
  }
  const job = jobs.get(path.split('/')[2]?.split('?')[0]);
  if (!job) throw new Error('操作不存在');
  if (method === 'POST' && path.endsWith('/cancel') && job.status === 'queued') job.status = 'cancelled';
  return structuredClone(job);
}
