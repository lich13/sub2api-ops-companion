import { useEffect, useState, useSyncExternalStore } from 'react';
import { ListChecks, X } from 'lucide-react';
import { api, command } from './bridge';
import type { Account } from './types';
import { useBackAction } from './mobile';
import { getOperations, subscribeOperations, setOperations, operationEpoch, submitOperation, type Operation } from './accountOperations';

const states: Record<string, string> = { queued: '已排队', running: '执行中', checking: '核对中', completed: '已完成', needs_confirmation: '需确认', failed: '失败', cancelled: '已取消', superseded: '已替代' };
const labels: Record<string, string> = { priority: '优先级', groups: '分组', recover: '恢复状态', usage: '额度操作', reset_quota: '使用重置卡', delete: '删除账号', test: '连接测试', schedulable: '调度', degradation_mark: '降智标记', model_test: '模型测试' };
function describe(value: Record<string, unknown>) {
  const names: Record<string, string> = { priority: '优先级', group_ids: '目标分组', scope_group_ids: '编辑分组', schedulable: '调度', marked: '降智标记', model_id: '模型', concurrency: '并发', status: '账号状态', name: '名称', type: '类型', action: '操作' };
  return Object.entries(value).filter(([key]) => key in names).map(([key, v]) => `${names[key]}：${typeof v === 'boolean' ? v ? '开启' : '关闭' : Array.isArray(v) ? v.map((id) => '#' + id).join('、') || '无' : String(v)}`).join('；') || '核对账号状态';
}
export function AccountOperationStatus({ id }: { id: number }) {
  const jobs = useSyncExternalStore(subscribeOperations, getOperations);
  const active = jobs.filter((job) => job.account_id === id && ['queued', 'running', 'checking', 'needs_confirmation'].includes(job.status));
  if (!active.length) return null;
  return <span className="operation-account-status">{active.some((job) => job.status === 'needs_confirmation') ? '操作需确认' : `${active.length} 项待办`}</span>;
}
export default function AccountOperations({ accounts, online, connectionKey, report, modelTest }: { accounts: Account[]; online: boolean; connectionKey: string; report: (error: unknown) => void; modelTest: (account: Account) => void }) {
  const jobs = useSyncExternalStore(subscribeOperations, getOperations);
  const [open, setOpen] = useState(false), [error, setError] = useState('');
  useBackAction(open, () => setOpen(false));
  useEffect(() => { setOpen(false); setError(''); }, [connectionKey]);
  useEffect(() => {
    if (!online) return;
    let alive = true, timer: ReturnType<typeof setTimeout>;
    const epoch = operationEpoch();
    const read = async () => {
      try {
        const next = await api<{items: Operation[]}>('GET', '/account-operations');
        if (alive && epoch === operationEpoch()) { setOperations(next.items); setError(''); }
      } catch (e) { if (alive && epoch === operationEpoch()) setError(String(e).replace(/^Error: /, '')); }
      if (alive) timer = setTimeout(() => void read(), open || getOperations().some((j) => ['queued', 'running', 'checking'].includes(j.status)) ? 2000 : 10000);
    };
    void read(); return () => { alive = false; clearTimeout(timer); };
  }, [online, connectionKey, open]);
  const pending = jobs.filter((j) => ['queued', 'running', 'checking', 'needs_confirmation'].includes(j.status));
  async function cancel(job: Operation) {
    try { const next = await api<Operation>('POST', `/account-operations/${job.id}/cancel`, {}); setOperations(getOperations().map((j) => j.id === next.id ? next : j)); }
    catch (e) { report(e); }
  }
  async function retry(job: Operation) {
    const account = accounts.find((a) => a.id === job.account_id);
    if (!account) return report('账号已删除或无法读取');
    if (job.action === 'model_test') { setOpen(false); modelTest(account); return; }
    try { await submitOperation(account, job.action, job.requested); await command('refresh'); }
    catch (e) { report(e); }
  }
  return <>{(jobs.length > 0 || error) && <button className="icon-button operation-entry" aria-label={`操作待办 ${pending.length}`} onClick={() => setOpen(true)}><ListChecks size={18}/>{pending.length > 0 && <span>{pending.length}</span>}</button>}
    {open && <div className="modal-backdrop" onClick={() => setOpen(false)}><section className="operation-dialog" role="dialog" aria-label="操作待办" aria-modal="true" onClick={(e) => e.stopPropagation()}><header><h2>操作待办</h2><button className="icon-button" aria-label="关闭待办" onClick={() => setOpen(false)}><X size={18}/></button></header>
      {error && <p className="bad-text" role="alert">{error}</p>}
      <div className="operation-list">{jobs.map((job) => <article key={job.id}><div><strong>{job.account_name} <span>#{job.account_id}</span></strong><span>{labels[job.action] || job.action} · {states[job.status] || job.status}</span></div>{job.reason && <p className={job.status === 'needs_confirmation' || job.status === 'failed' ? 'bad-text' : 'muted'}>{job.reason}</p>}
        {job.status === 'needs_confirmation' && <><dl><div><dt>请求</dt><dd>{describe(job.requested)}</dd></div>{job.current && <div><dt>当前</dt><dd>{describe(job.current)}</dd></div>}</dl><button disabled={!online} onClick={() => void retry(job)}>确认后重新提交</button></>}
        {['queued', 'needs_confirmation'].includes(job.status) || job.status === 'running' && ['test', 'model_test'].includes(job.action) ? <button disabled={!online} onClick={() => void cancel(job)}>取消</button> : null}
      </article>)}</div></section></div>}
  </>;
}
