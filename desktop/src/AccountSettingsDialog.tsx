import { useEffect, useRef, useState } from "react";
import { ArrowRight, LoaderCircle, Plus, RefreshCw, Save, X } from "lucide-react";
import { api, command } from "./bridge";
import { followOperation, operationEpoch, submitOperation, type Operation } from "./accountOperations";
import { useBackAction } from "./mobile";
import { useDialogFocus } from "./dialogFocus";
import type { Account } from "./types";
import { validate as validateProfile, type Profile } from "./AccountTemplates";
import "./account-settings.css";

export type SettingsSection = "whitelist" | "mappings" | "proxy";
export type AccountSettings = {
  account_id: number; priority: number; concurrency: number | null; load_factor: number | null; effective_load_factor: number;
  proxy_id: number | null; model_config: Profile;
  capabilities: Record<string, { editable: boolean; reason: string }>;
  versions: Record<string, string>;
};
type ProxyOption = { id: number; name: string; protocol: string; status: string };
type SyncResult = { models: string[]; warnings: string[]; synced_at: string };
export const operationLabel = (job: Operation) => ({ queued: "已排队，等待账号空闲", running: "正在保存", checking: "正在核对实际值", completed: "已确认生效", needs_confirmation: "相关字段已变化，请在待办中核对", failed: "保存失败", cancelled: "已取消", superseded: "已由新请求替代" } as Record<string, string>)[job.status] || job.status;

export default function AccountSettingsDialog({ account, initialSection, online, close }: {
  account: Account; initialSection: SettingsSection; online: boolean; close: () => void;
}) {
  const [settings, setSettings] = useState<AccountSettings | null>(null), [section, setSection] = useState(initialSection);
  const [profile, setProfile] = useState<Profile>({ whitelist: [], mappings: [] }), [proxyId, setProxyId] = useState<number | null>(null);
  const [proxies, setProxies] = useState<ProxyOption[]>([]), [proxyReady, setProxyReady] = useState(false), [proxyRefresh, setProxyRefresh] = useState(0);
  const [query, setQuery] = useState(""), [catalog, setCatalog] = useState<SyncResult | null>(null), [selected, setSelected] = useState(new Set<string>());
  const [busy, setBusy] = useState(false), [syncing, setSyncing] = useState(false), [loading, setLoading] = useState(true), [error, setError] = useState(""), [notice, setNotice] = useState("");
  const generation = useRef(0), submitGuard = useRef(false), syncGuard = useRef(false);
  const dialog = useRef<HTMLElement>(null);
  useDialogFocus(dialog);
  const isProxy = section === "proxy", action = isProxy ? "proxy" : "model_config";
  useBackAction(true, close);
  useEffect(() => { const key = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); close(); } }; document.addEventListener("keydown", key); return () => document.removeEventListener("keydown", key); }, [close]);
  async function read() {
    const current = ++generation.current;
    setLoading(true); setError("");
    try {
      const result = await api<AccountSettings>("GET", `/accounts/${account.id}/settings`);
      if (current !== generation.current) return;
      setSettings(result); setProfile(structuredClone(result.model_config)); setProxyId(result.proxy_id);
    } catch (e) { if (current === generation.current) setError(String(e).replace(/^Error: /, "")); }
    finally { if (current === generation.current) setLoading(false); }
  }
  useEffect(() => { void read(); return () => { ++generation.current; }; }, [account.id]);
  useEffect(() => {
    if (!isProxy) return;
    let cancelled = false;
    setProxyReady(false);
    api<ProxyOption[]>("GET", "/proxies").then(value => { if (!cancelled) { setProxies(value); setProxyReady(true); } })
      .catch(e => { if (!cancelled) setError(String(e).replace(/^Error: /, "")); });
    return () => { cancelled = true; };
  }, [isProxy, proxyRefresh]);
  const dirty = !!settings && (isProxy ? proxyId !== settings.proxy_id : JSON.stringify(profile) !== JSON.stringify(settings.model_config));
  const invalid = isProxy ? "" : validateProfile(profile);
  const writable = !!settings?.capabilities[action]?.editable;
  async function save() {
    if (!settings || !online || !writable || !dirty || invalid || submitGuard.current) return;
    submitGuard.current = true; setBusy(true); setError(""); setNotice("");
    const current = generation.current, epoch = operationEpoch();
    const submitted = structuredClone(profile), submittedProxy = proxyId;
    try {
      const job = await submitOperation(account, action, isProxy ? { proxy_id: proxyId } : profile, settings.versions[action]);
      if (current === generation.current) setNotice(operationLabel(job));
      const result = await followOperation<{ verified: boolean; field_version?: string; settings_versions?: Record<string, string> }>(job, undefined, next => {
        if (current === generation.current) setNotice(operationLabel(next));
      });
      if (epoch === operationEpoch()) void command("refresh");
      if (current !== generation.current) return;
      if (!result.verified) throw new Error("保存尚未确认，请在待办中查看");
      setSettings(old => old ? { ...old, ...(action === "proxy" ? { proxy_id: submittedProxy } : { model_config: submitted }), versions: { ...old.versions, ...result.settings_versions, ...(result.field_version ? { [action]: result.field_version } : {}) } } : old);
      setNotice("已确认生效");
    } catch (e) { if (current === generation.current) { setNotice(""); setError(String(e).replace(/^Error: /, "")); } }
    finally { submitGuard.current = false; if (current === generation.current) setBusy(false); }
  }
  async function sync() {
    if (!settings || !online || syncGuard.current) return;
    const current = generation.current;
    syncGuard.current = true; setSyncing(true); setError(""); setCatalog(null); setSelected(new Set()); setNotice("");
    try {
      const job = await submitOperation(account, "sync_models", {}, settings.versions.sync_models);
      const result = await followOperation<SyncResult>(job, undefined, next => {
        if (current === generation.current) setNotice(next.status === "queued" ? "同步已排队" : next.status === "running" ? "正在同步上游模型" : operationLabel(next));
      });
      if (current === generation.current) { setCatalog(result); setNotice(""); }
    } catch (e) { if (current === generation.current) { setNotice(""); setError(String(e).replace(/^Error: /, "")); } }
    finally { syncGuard.current = false; if (current === generation.current) setSyncing(false); }
  }
  const sourceSet = new Set(profile.mappings.map(r => r.source));
  const present = new Set(profile.whitelist);
  const candidates = catalog?.models.filter(m => m.toLowerCase().includes(query.trim().toLowerCase())) ?? [];
  const missing = candidates.filter(m => !present.has(m) && !sourceSet.has(m));
  function addSelected() {
    const additions = (catalog?.models ?? []).filter(m => selected.has(m) && !present.has(m) && !sourceSet.has(m));
    if (profile.whitelist.length + additions.length > 300) { setError("白名单最多 300 项"); return; }
    setProfile({ ...profile, whitelist: [...profile.whitelist, ...additions] }); setSelected(new Set());
  }
  const proxyRows = proxies.filter(p => `${p.name} ${p.id} ${p.protocol}`.toLowerCase().includes(query.trim().toLowerCase()));
  return <div className="modal-backdrop" onClick={close}><section ref={dialog} className="account-settings-dialog" role="dialog" aria-modal="true" aria-label="账号设置" onClick={e => e.stopPropagation()}>
    <header><div><h2>{isProxy ? "选择代理" : "模型配置"}</h2><span title={account.name}>{account.name} · #{account.id}</span></div><button className="icon-button" aria-label="关闭账号设置" onClick={close}><X size={18}/></button></header>
    <div className="account-settings-body">
      {!isProxy && <div className="settings-tabs" role="tablist" aria-label="模型配置区域">
        {([['whitelist', '模型白名单'], ['mappings', '模型映射']] as const).map(([id, label]) => <button type="button" key={id} role="tab" aria-selected={section === id} onClick={() => { setSection(id); setQuery(""); }}>{label}</button>)}
      </div>}
      {loading ? <p role="status"><LoaderCircle className="spin" size={16}/>正在读取</p> : !settings ? <button onClick={() => void read()}>重新读取</button> : <>
        {!writable && <p className="settings-notice">{settings.capabilities[action]?.reason || "该账号不支持此项修改"}</p>}
        {isProxy ? <>
          <div className="settings-proxy-search"><input className="settings-search" type="search" aria-label="搜索代理" placeholder="搜索代理名称或 ID" value={query} onChange={e => setQuery(e.target.value)}/><button aria-label="刷新代理" disabled={busy || !online} onClick={() => { setError(""); setProxyRefresh(n => n + 1); }}><RefreshCw size={15}/></button></div>
          <div className="proxy-options" role="radiogroup" aria-label="账号代理">
            <label><input type="radio" name="proxy" checked={proxyId === null} disabled={!writable || busy} onChange={() => setProxyId(null)}/><span>不使用代理</span></label>
            {proxyId !== null && !proxies.some(p => p.id === proxyId) && <p className="muted">当前代理 #{proxyId} · 不在可用列表中</p>}
            {proxyRows.map(p => <label key={p.id}><input type="radio" name="proxy" checked={proxyId === p.id} disabled={!writable || busy || p.status !== "active"} onChange={() => setProxyId(p.id)}/><span><b>{p.name}</b><small>#{p.id} · {p.protocol} · {p.status === "active" ? "可用" : "不可用"}</small></span></label>)}
          </div>
          {!proxyReady && <p className="muted">代理列表尚未读取完成</p>}
        </> : <>
          {!profile.whitelist.length && !profile.mappings.length && <p className="settings-notice">不限制模型</p>}
          {section === "whitelist" ? <>
            <div className="settings-section-tools"><h3>放行模型</h3><button disabled={!writable || busy} onClick={() => setProfile({ ...profile, whitelist: [...profile.whitelist, ""] })}><Plus size={14}/>添加模型</button><button disabled={!online || busy || syncing || !settings.capabilities.sync_models?.editable} title={settings.capabilities.sync_models?.reason} onClick={() => void sync()}><RefreshCw size={14} className={syncing ? "spin" : ""}/>同步上游模型</button></div>
            {profile.whitelist.map((model, i) => <div className="settings-model-row" key={i}><input aria-label={`白名单模型 ${i + 1}`} value={model} disabled={!writable || busy} onChange={e => setProfile({ ...profile, whitelist: profile.whitelist.map((m, n) => n === i ? e.target.value.trim() : m) })}/><button className="icon-button" aria-label={`移除白名单 ${i + 1}`} disabled={!writable || busy} onClick={() => setProfile({ ...profile, whitelist: profile.whitelist.filter((_, n) => n !== i) })}><X size={15}/></button></div>)}
            {catalog && <section className="settings-catalog" aria-label="同步模型候选">
              <input type="search" aria-label="搜索支持模型" placeholder="搜索支持模型" value={query} onChange={e => setQuery(e.target.value)}/>
              <div className="settings-section-tools"><span>{catalog.models.length} 个模型</span><button disabled={busy || !writable || !missing.length} onClick={() => setSelected(new Set([...selected, ...missing]))}>全选缺少项</button><button disabled={busy || !writable || !selected.size} onClick={addSelected}>添加所选</button></div>
              {catalog.warnings.map(w => <p className="muted" key={w}>{w}</p>)}
              <div className="settings-model-options">{candidates.map(m => <label key={m}><input type="checkbox" checked={selected.has(m)} disabled={busy || !writable || present.has(m) || sourceSet.has(m)} onChange={e => setSelected(old => { const next = new Set(old); if (e.target.checked) next.add(m); else next.delete(m); return next; })}/><span>{m}</span>{(present.has(m) || sourceSet.has(m)) && <small>{sourceSet.has(m) ? "与映射源冲突" : "已添加"}</small>}</label>)}</div>
            </section>}
          </> : <>
            <div className="settings-section-tools"><h3>请求模型 → 转发模型</h3><button disabled={!writable || busy} onClick={() => setProfile({ ...profile, mappings: [...profile.mappings, { source: "", target: "" }] })}><Plus size={14}/>添加映射</button></div>
            {profile.mappings.map((rule, i) => <div className="settings-model-row mapping" key={i}><input aria-label={`请求模型 ${i + 1}`} placeholder="请求模型" value={rule.source} disabled={!writable || busy} onChange={e => setProfile({ ...profile, mappings: profile.mappings.map((r, n) => n === i ? { ...r, source: e.target.value.trim() } : r) })}/><ArrowRight size={14}/><input aria-label={`转发模型 ${i + 1}`} placeholder="转发模型" value={rule.target} disabled={!writable || busy} onChange={e => setProfile({ ...profile, mappings: profile.mappings.map((r, n) => n === i ? { ...r, target: e.target.value.trim() } : r) })}/><button className="icon-button" aria-label={`移除映射 ${i + 1}`} disabled={!writable || busy} onClick={() => setProfile({ ...profile, mappings: profile.mappings.filter((_, n) => n !== i) })}><X size={15}/></button></div>)}
          </>}
        </>}
      </>}
      {(error || invalid) && <p className="bad-text settings-notice" role="alert">{error || invalid}</p>}
      {notice && <p className="settings-notice" role="status">{notice}</p>}
    </div>
    <footer><button disabled={!dirty || busy} onClick={() => { if (settings) { setProfile(structuredClone(settings.model_config)); setProxyId(settings.proxy_id); setError(""); setNotice(""); } }}>放弃修改</button><button className="primary" disabled={!online || busy || !writable || !dirty || !!invalid || (isProxy && proxyId !== null && !proxyReady)} onClick={() => void save()}>{busy ? <LoaderCircle className="spin" size={15}/> : <Save size={15}/>}保存</button></footer>
  </section></div>;
}
