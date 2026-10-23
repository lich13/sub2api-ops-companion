import { useEffect, useRef, useState } from "react";
import { LoaderCircle, RefreshCw, X } from "lucide-react";
import { api, command } from "./bridge";
import { clientId, getOperations, operationEpoch, setOperations, type Operation } from "./accountOperations";
import { operationLabel, type AccountSettings } from "./AccountSettingsDialog";
import { useBackAction } from "./mobile";
import { useDialogFocus } from "./dialogFocus";
import type { Account } from "./types";
import "./account-settings.css";
import ProxyOptions from "./ProxyOptions";

type Proxy = { id: number; name: string; protocol: string; status: string };
type Batch = { batch_id: string; items: Operation[]; pending: number };
const pending = (batch: Batch | null) => batch?.items.some(j => ["queued", "running", "checking"].includes(j.status)) ?? false;

export default function ProxyBatchDialog({ accounts, online, connectionKey, close, report }: {
  accounts: Account[]; online: boolean; connectionKey: string; close: () => void; report: (error: unknown) => void;
}) {
  const [settings, setSettings] = useState<Record<number, AccountSettings>>({}), [errors, setErrors] = useState<Record<number, string>>({});
  const [proxies, setProxies] = useState<Proxy[]>([]), [proxyId, setProxyId] = useState<number | null | undefined>(undefined);
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const [batch, setBatch] = useState<Batch | null>(null), [batchError, setBatchError] = useState("");
  const dialog = useRef<HTMLElement>(null), generation = useRef(0), guard = useRef(false);
  const request = useRef<{ signature: string; request_id: string } | null>(null);
  useDialogFocus(dialog); useBackAction(true, close);
  useEffect(() => { const key = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); close(); } }; document.addEventListener("keydown", key); return () => document.removeEventListener("keydown", key); }, [close]);
  async function read() {
    const token = ++generation.current, epoch = operationEpoch();
    setLoading(true); setError(""); setSettings({}); setErrors({}); setBatch(null); setBatchError(""); request.current = null;
    const results = await Promise.allSettled([api<Proxy[]>("GET", "/proxies"), ...accounts.map(a => api<AccountSettings>("GET", `/accounts/${a.id}/settings`))]);
    if (token !== generation.current || epoch !== operationEpoch()) return;
    if (results[0].status === "fulfilled") setProxies(results[0].value as Proxy[]);
    else setError(String(results[0].reason).replace(/^Error: /, ""));
    const next: Record<number, AccountSettings> = {}, failed: Record<number, string> = {};
    results.slice(1).forEach((r, i) => { if (r.status === "fulfilled") next[accounts[i].id] = r.value as AccountSettings; else failed[accounts[i].id] = String(r.reason).replace(/^Error: /, ""); });
    setSettings(next); setErrors(failed); setLoading(false);
  }
  useEffect(() => {
    guard.current = false; setBusy(false); setProxyId(undefined);
    void read(); return () => { ++generation.current; };
  }, [connectionKey, accounts]);
  function accept(value: Batch) {
    setBatch(value);
    const ids = new Set(value.items.map(j => j.id));
    setOperations([...value.items, ...getOperations().filter(j => !ids.has(j.id))]);
    if (value.items.length > 0 && value.items.every(j => j.status === "completed")) close();
  }
  async function readBatch(id: string) {
    const token = generation.current, epoch = operationEpoch();
    try {
      const value = await api<Batch>("GET", `/account-operations?batch_id=${id}`);
      if (token !== generation.current || epoch !== operationEpoch()) return;
      accept(value); setBatchError(""); if (!pending(value)) void command("refresh").catch(report);
    } catch (e) { if (token === generation.current && epoch === operationEpoch()) setBatchError(String(e).replace(/^Error: /, "")); }
  }
  useEffect(() => {
    if (!online || !batch || !pending(batch) || batchError) return;
    const timer = setTimeout(() => void readBatch(batch.batch_id), 2000);
    return () => clearTimeout(timer);
  }, [batch, batchError, online]);
  const editable = accounts.filter(a => settings[a.id]?.capabilities.proxy.editable);
  const targetValid = proxyId === null || proxies.some(p => p.id === proxyId && p.status === "active");
  async function apply() {
    if (guard.current || !online || loading || !targetValid || proxyId === undefined || !editable.length) return;
    guard.current = true; setBusy(true); setError("");
    const token = generation.current, epoch = operationEpoch();
    const payload = { action: "proxy", payload: { proxy_id: proxyId }, client_id: clientId(),
      accounts: accounts.filter(a => settings[a.id]).map(a => ({ account_id: a.id, expected_version: settings[a.id].versions.proxy })) };
    const signature = JSON.stringify(payload);
    if (request.current?.signature !== signature) request.current = { signature, request_id: crypto.randomUUID() };
    try {
      const value = await api<Batch>("POST", "/account-operations/batch", { ...payload, request_id: request.current.request_id });
      if (token !== generation.current || epoch !== operationEpoch()) return;
      accept(value); setBatchError(""); if (!pending(value)) void command("refresh").catch(report);
    } catch (e) { if (token === generation.current && epoch === operationEpoch()) { setError(String(e).replace(/^Error: /, "")); report(e); } }
    finally { if (token === generation.current) { guard.current = false; setBusy(false); } }
  }
  const name = (id: number | null | undefined) => id === undefined ? "请选择代理" : id === null ? "不使用代理" : `${proxies.find(p => p.id === id)?.name ?? "代理"} #${id}`;
  const locked = busy || !!batch;
  return <div className="modal-backdrop" onClick={close}><section ref={dialog} className="account-settings-dialog proxy-batch-dialog" role="dialog" aria-modal="true" aria-label="批量选择代理" onClick={e => e.stopPropagation()}>
    <header><div><h2>选择代理</h2><span>已选择 {accounts.length} 个账号</span></div><button className="icon-button" aria-label="关闭代理选择" onClick={close}><X size={18}/></button></header>
    <div className="account-settings-body">
      <button aria-label="刷新代理预览" disabled={busy || loading || pending(batch) || !online} onClick={() => void read()}><RefreshCw size={15}/>刷新预览</button>
      {loading ? <p role="status"><LoaderCircle className="spin" size={15}/>正在读取</p> : <>
        <ProxyOptions proxies={proxies} value={proxyId} onChange={setProxyId} online={online} disabled={locked} connectionKey={connectionKey}/>
        <p className="settings-notice muted">母账号的代理变更同时影响继承其代理的子账号。</p>
        <div className="proxy-batch-previews">{accounts.map(account => {
          const value = settings[account.id], job = batch?.items.find(j => j.account_id === account.id);
          return <article key={account.id}><header><strong>{account.name} <span className="muted">#{account.id}</span></strong>{job && <span>{operationLabel(job)}</span>}</header>
            {value ? <><dl><dt>当前</dt><dd>{name(value.proxy_id)}</dd><dt>目标</dt><dd>{name(proxyId)}{proxyId !== undefined && value.proxy_id === proxyId && " · 无变化"}</dd></dl>
              {!value.capabilities.proxy.editable && <p className="muted">{value.capabilities.proxy.reason}</p>}
            </> : <p className="bad-text">{errors[account.id] || "账号读取失败"}</p>}
            {job?.reason && <p className={job.status === "failed" || job.status === "needs_confirmation" ? "bad-text" : "muted"}>{job.reason}</p>}
            {job?.status === "queued" && <button onClick={() => { const token = generation.current, epoch = operationEpoch(); void api("POST", `/account-operations/${job.id}/cancel`).then(() => { if (token === generation.current && epoch === operationEpoch() && batch) void readBatch(batch.batch_id); }).catch(e => { if (token === generation.current && epoch === operationEpoch()) report(e); }); }}>取消</button>}
          </article>;
        })}</div>
      </>}
      {error && <p role="alert" className="bad-text">{error}</p>}
      {batchError && <p role="alert" className="bad-text">{batchError}<button onClick={() => batch && void readBatch(batch.batch_id)}>重试读取</button></p>}
    </div>
    <footer><button onClick={close}>关闭</button>{batch ? <button disabled={busy || pending(batch) || !online} onClick={() => void read()}>重新预览</button> : <button className="primary" disabled={!online || busy || loading || !targetValid || !editable.length || (!!error && !request.current) || Object.keys(errors).length > 0} onClick={() => void apply()}>{busy && <LoaderCircle className="spin" size={15}/>}确认应用</button>}</footer>
  </section></div>;
}
