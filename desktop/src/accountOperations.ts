import { api, command } from './bridge';
import type { Account, TestEvent } from './types';

export type Operation = {
  id: string; account_id: number; account_name: string; action: string; status: string;
  reason?: string; requested: Record<string, unknown>; current?: Record<string, unknown>;
  result?: Record<string, unknown>; events?: TestEvent[]; next_event?: number;
};
export const operationTerminal = new Set(['completed', 'failed', 'cancelled', 'superseded', 'needs_confirmation']);
const listeners = new Set<() => void>();
let rows: Operation[] = [], epoch = 0, connection = '';
const clientKey = 'sub2ops-operation-client';
function clientId() {
  const storage = typeof globalThis.localStorage === 'undefined' ? null : globalThis.localStorage;
  let id = storage?.getItem(clientKey) ?? null;
  if (!id) { id = crypto.randomUUID(); storage?.setItem(clientKey, id); }
  return id;
}
export function bindOperationConnection(key: string) {
  if (connection === key) return;
  connection = key; ++epoch; setOperations([]);
}
export const getOperations = () => rows;
export function subscribeOperations(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; }
export function setOperations(next: Operation[]) { rows = next; listeners.forEach((listener) => listener()); }
export const operationEpoch = () => epoch;
export async function submitOperation(account: Account, action: string, payload: Record<string, unknown>, expected?: string): Promise<Operation> {
  const current = epoch;
  const version = expected ?? (action === 'degradation_mark' ? account.degradation_mark?.version : account.operation_versions?.[action]);
  if (!version) throw new Error('请刷新账号后重试');
  const job = await api<Operation>('POST', `/accounts/${account.id}/operations`, {
    action, payload, expected_version: version, request_id: crypto.randomUUID(), client_id: clientId(),
  });
  if (current !== epoch) throw new Error('连接已切换；请求保留在原连接');
  setOperations([job, ...rows.filter((row) => row.id !== job.id)]);
  return job;
}
export async function followOperation<T>(initial: Operation, onEvent?: (event: TestEvent) => void): Promise<T> {
  const current = epoch;
  let job = initial, after = 0;
  while (true) {
    if (current !== epoch) throw new Error('连接已切换；请求保留在原连接');
    if (onEvent) {
      for (const event of job.events ?? []) onEvent(event);
      after = job.next_event ?? after;
    }
    if (operationTerminal.has(job.status)) {
      // Drain all remaining connection-test events before presenting completion.
      if (onEvent && (job.events?.length ?? 0) === 100) {
        job = await api<Operation>('GET', `/account-operations/${job.id}?after_event=${after}`);
        continue;
      }
      if (job.status === 'completed') return job.result as T;
      throw new Error(job.reason || ({ needs_confirmation: '请求已保留，请在待办中核对', cancelled: '已取消', superseded: '已由新请求替代' } as Record<string, string>)[job.status] || '操作失败');
    }
    onEvent?.({ type: 'status', text: job.status === 'queued' ? '已排队，等待账号空闲' : job.status === 'checking' ? '核对结果' : '测试中' });
    await new Promise((resolve) => setTimeout(resolve, 500));
    if (current !== epoch) throw new Error('连接已切换；请求保留在原连接');
    const next = await api<Operation>('GET', `/account-operations/${job.id}${onEvent ? `?after_event=${after}` : ''}`);
    if (current !== epoch) throw new Error('连接已切换；请求保留在原连接');
    job = next;
    setOperations(rows.some((r) => r.id === job.id) ? rows.map((r) => r.id === job.id ? job : r) : [job, ...rows]);
  }
}
export async function accountOperation<T>(account: Account, action: string, payload: Record<string, unknown>, expected?: string): Promise<T> {
  if (!account.operation_versions && action !== 'degradation_mark') {
    const suffix = ({ recover: 'recover-state', usage: 'usage-action', reset_quota: 'usage-action' } as Record<string, string>)[action] || action;
    return api<T>(action === 'groups' ? 'PUT' : action === 'delete' ? 'DELETE' : 'POST', `/accounts/${account.id}${action === 'delete' ? '' : `/${suffix}`}`, { ...payload, expected_version: expected || account.version });
  }
  if (!account.operation_versions && action === 'degradation_mark') return api<T>('PUT', `/accounts/${account.id}/degradation-mark`, { ...payload, expected_mark_version: expected || account.degradation_mark?.version });
  const job = await submitOperation(account, action, payload, expected);
  const result = await followOperation<T>(job);
  void command('refresh');
  return result;
}
