import { useEffect, useRef, useState } from 'react';
import { LoaderCircle, RefreshCw, Square, X } from 'lucide-react';
import { api, command } from './bridge';
import { clientId, getOperations, setOperations, type Operation, type OperationBatch } from './accountOperations';
import type { Account } from './types';
import './connection-batch.css';

type Submission = { action: 'test'; payload: { confirmed: true }; accounts: { account_id: number; expected_version: string }[]; request_id: string; client_id: string };
type Saved = { batchId?: string; request?: Submission; visible?: boolean };
const running = (batch: OperationBatch | null) => batch?.items.some(j => ['queued', 'running', 'checking'].includes(j.status)) ?? false;
const message = (e: unknown) => String(e).replace(/^Error: /, '');
function storageName(scope: string) {
  let hash = 14695981039346656037n;
  for (const ch of scope) hash = BigInt.asUintN(64, (hash ^ BigInt(ch.codePointAt(0)!)) * 1099511628211n);
  return `sub2ops-connection-batch-${hash.toString(16)}`;
}
function readSaved(key: string): Saved {
  try {
    const value = JSON.parse(localStorage.getItem(key) || '{}') as Saved;
    if (value.batchId && /^[a-f0-9]{32}$/.test(value.batchId)) return { batchId: value.batchId, visible: value.visible !== false };
    const req = value.request;
    if (req?.action === 'test' && req.payload?.confirmed === true && /^[A-Za-z0-9-]{16,64}$/.test(req.request_id)
      && /^[A-Za-z0-9-]{16,64}$/.test(req.client_id) && Array.isArray(req.accounts) && req.accounts.length > 0 && req.accounts.length <= 1000
      && req.accounts.every(a => Number.isSafeInteger(a.account_id) && a.account_id > 0 && /^[a-f0-9]{64}$/.test(a.expected_version))) return { request: req, visible: value.visible !== false };
  } catch { /* An unreadable preference never starts an operation. */ }
  return {};
}
function remember(key: string, saved: Saved) { localStorage.setItem(key, JSON.stringify(saved)); }
function publish(batch: OperationBatch) { setOperations([...batch.items, ...getOperations().filter(row => !batch.items.some(item => row.id === item.id))]); }

export function useConnectionBatch({ connectionKey, storageKey, online, active }: {
  connectionKey: string; storageKey: string; online: boolean; active: boolean;
}) {
  const key = storageName(storageKey);
  const [view, setView] = useState<{ scope: string; batch: OperationBatch | null }>({ scope: connectionKey, batch: null });
  const [batchId, setBatchId] = useState<string | null>(null), [starting, setStarting] = useState(false), [stopping, setStopping] = useState(false);
  const [error, setError] = useState(''), [refresh, setRefresh] = useState(0);
  const [visible, setVisible] = useState(true);
  const visibility = useRef(true);
  const generation = useRef(0), revision = useRef(0), guard = useRef(false), submission = useRef<Submission | null>(null);
  const batch = view.scope === connectionKey ? view.batch : null;
  useEffect(() => {
    ++generation.current; ++revision.current; guard.current = false;
    const saved = readSaved(key); submission.current = saved.request ?? null;
    visibility.current = saved.visible !== false; setVisible(visibility.current);
    setView({ scope: connectionKey, batch: null }); setBatchId(saved.batchId ?? null); setStarting(false); setStopping(false);
    setError(saved.request ? '上次提交结果未确认，请恢复提交结果' : '');
    return () => { ++generation.current; ++revision.current; };
  }, [connectionKey, key]);

  useEffect(() => {
    if (!online || !active || !batchId || view.scope !== connectionKey) return;
    let alive = true, timer: ReturnType<typeof setTimeout> | undefined;
    const token = generation.current;
    async function read() {
      const version = revision.current;
      try {
        const next = await api<OperationBatch>('GET', `/account-operations?batch_id=${batchId}`);
        if (!alive || token !== generation.current || version !== revision.current) return;
        setView({ scope: connectionKey, batch: next }); publish(next); setError('');
        if (running(next)) timer = setTimeout(() => void read(), 1000);
        else void command('refresh');
      } catch (e) {
        if (alive && token === generation.current && version === revision.current) setError(message(e));
      }
    }
    void read();
    return () => { alive = false; clearTimeout(timer); };
  }, [connectionKey, batchId, online, active, refresh, view.scope]);

  async function send(request: Submission) {
    if (!online || guard.current) return;
    guard.current = true; setStarting(true); setError('');
    const token = generation.current; ++revision.current;
    try {
      remember(key, { request, visible: visibility.current }); submission.current = request;
      const next = await api<OperationBatch>('POST', '/account-operations/batch', request);
      if (token !== generation.current) return;
      remember(key, { batchId: next.batch_id, visible: visibility.current }); submission.current = null;
      setView({ scope: connectionKey, batch: next }); setBatchId(next.batch_id); publish(next);
    } catch (e) {
      if (token === generation.current) setError(message(e));
    } finally {
      if (token === generation.current) { guard.current = false; setStarting(false); }
    }
  }
  async function start(accounts: Account[]) {
    if (!online || guard.current || running(batch) || batchId && !batch || submission.current || !accounts.length) return;
    visibility.current = true; setVisible(true);
    if (accounts.length > 1000) { setError('每批最多测试 1000 个账号'); return; }
    const request: Submission = { action: 'test', payload: { confirmed: true },
      accounts: accounts.map(account => ({ account_id: account.id,
        expected_version: account.operation_versions?.test ?? account.version })),
      request_id: crypto.randomUUID(), client_id: clientId() };
    await send(request);
  }
  async function retry() {
    if (submission.current) await send(submission.current);
    else setRefresh(value => value + 1);
  }
  function changeVisibility(next: boolean) {
    visibility.current = next; setVisible(next);
    try {
      remember(key, { ...(submission.current ? { request: submission.current } : batchId ? { batchId } : {}), visible: next });
    } catch { /* Closing a result must remain possible when storage is unavailable. */ }
  }
  async function stop() {
    if (!online || !batchId || stopping) return;
    const token = generation.current; ++revision.current; setStopping(true); setError('');
    try {
      const next = await api<OperationBatch>('POST', `/account-operations/batches/${batchId}/cancel`, {});
      if (token !== generation.current) return;
      setView({ scope: connectionKey, batch: next }); publish(next);
    } catch (e) { if (token === generation.current) setError(message(e)); }
    finally { if (token === generation.current) { setStopping(false); setRefresh(value => value + 1); } }
  }
  return { batch, starting, stopping, error, online, visible,
    hasResult: !!batchId || !!submission.current || !!batch || starting || !!error,
    busy: starting || running(batch) || !!(batchId && !batch) || !!submission.current,
    recoverSubmission: !!submission.current, start, stop, retry, close: () => changeVisibility(false), show: () => changeVisibility(true) };
}
export type ConnectionBatchControl = ReturnType<typeof useConnectionBatch>;

function status(job: Operation) {
  if (job.status === 'completed') return job.result?.success === true ? '测试通过' : '测试失败';
  return ({ queued: '等待执行', running: '测试中', checking: '核对结果', needs_confirmation: '待核对',
    failed: '未执行', cancelled: '已停止', superseded: '已替代' } as Record<string, string>)[job.status] ?? '待核对';
}
export function ConnectionBatchResults({ control }: { control: ConnectionBatchControl }) {
  const { batch, starting, stopping, error, online } = control;
  if (!control.hasResult) return null;
  const items = batch?.items ?? [], unfinished = items.filter(j => ['queued', 'running', 'checking'].includes(j.status)).length;
  const passed = items.filter(j => j.status === 'completed' && j.result?.success === true).length;
  const canStop = items.some(j => ['queued', 'needs_confirmation'].includes(j.status) && !j.started_at);
  const progress = starting ? '正在提交' : !batch ? control.recoverSubmission ? '提交结果待确认' : '等待读取结果'
    : `${unfinished ? '已完成' : '已结束'} ${items.length - unfinished}/${items.length} · 通过 ${passed}`;
  if (!control.visible) return <section className="connection-batch connection-batch-collapsed" aria-label="批量测试连接摘要">
    <button onClick={control.show}>查看测活结果</button><span role="status">{progress}</span>
  </section>;
  return <section className="connection-batch" aria-label="批量测试连接">
    <header><strong>测试连接</strong><span role="status">{progress}</span><div className="connection-batch-actions">
      {canStop && <button disabled={!online || stopping} onClick={() => void control.stop()}>{stopping ? <LoaderCircle size={12} className="spin"/> : <Square size={12}/>}停止未开始</button>}
      <button onClick={control.close}><X size={12}/>关闭结果</button></div>
    </header>
    {error && <div className="connection-batch-error" role="alert"><span>{error}</span><button disabled={!online || starting} onClick={() => void control.retry()}><RefreshCw size={12}/>{control.recoverSubmission ? '恢复提交结果' : '重新读取'}</button></div>}
    {items.length > 0 && <div className="connection-batch-scroll"><table>
      <thead><tr><th>账号</th><th>结果</th><th>模型</th><th>耗时</th><th>反馈</th></tr></thead>
      <tbody>{items.map(job => {
        const requested = String(job.result?.requested_model ?? job.requested.model_id ?? '') || '原生默认';
        const actual = typeof job.result?.actual_model === 'string' ? job.result.actual_model : '';
        const duration = job.result?.duration_ms;
        return <tr key={job.id}>
          <td title={job.account_name}>{job.account_name}</td>
          <td className={job.result?.success === true ? 'good-text' : ['failed', 'needs_confirmation'].includes(job.status) || job.status === 'completed' ? 'bad-text' : ''}>{job.status === 'running' && <LoaderCircle size={12} className="spin"/>}{status(job)}</td>
          <td>{requested}{actual && actual !== requested ? <> → {actual}</> : !actual && job.result ? <> → 未返回</> : null}</td>
          <td>{typeof duration === 'number' ? `${(duration / 1000).toFixed(2)} s` : '—'}</td>
          <td>{String(job.result?.error || job.reason || '—')}</td>
        </tr>;
      })}</tbody>
    </table></div>}
  </section>;
}
