import { useEffect, useRef, useState } from "react";
import { ArrowRight, LoaderCircle, Plus, RefreshCw, Save, Trash2, X } from "lucide-react";
import { api, command } from "./bridge";
import { clientId, getOperations, operationEpoch, setOperations, type Operation } from "./accountOperations";
import { useBackAction } from "./mobile";
import type { Account } from "./types";

type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
type Config = { version: string; configured: boolean; templates: Record<string, Profile>; template_meta: Record<string, { name: string }>; template_order: string[]; template_versions?: Record<string, string> };
type Draft = Profile & { name: string };
type AccountConfig = { account?: { id: number; eligible: boolean; passthrough: boolean; version: string; config: Profile; reason?: string } };
type Batch = { batch_id: string; items: Operation[]; pending: number };
const emptyProfile = (): Profile => ({ whitelist: [], mappings: [] });
const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
const draftsOf = (value: Config): Record<string, Draft> => Object.fromEntries(value.template_order.map(id => [id, { name: value.template_meta[id].name, ...structuredClone(value.templates[id]) }]));
const profileOf = (value: Profile): Profile => ({ whitelist: value.whitelist, mappings: value.mappings });
const running = (batch: Batch | null) => batch?.items.some(item => ["queued", "running", "checking"].includes(item.status)) ?? false;
const statusNames: Record<string, string> = { queued: "已排队", running: "执行中", checking: "核对中", completed: "已完成", needs_confirmation: "需确认", failed: "失败", cancelled: "已取消", superseded: "已替代" };

function validate(profile: Profile) {
  const seen = new Set<string>();
  for (const model of profile.whitelist) {
    if (!/^[A-Za-z0-9._:/-]{1,200}$/.test(model)) return "白名单必须是精确模型 ID";
    if (seen.has(model)) return `重复的模型：${model}`;
    seen.add(model);
  }
  for (const rule of profile.mappings) {
    if (!/^(?:[A-Za-z0-9._:/-]{1,199}\*?|\*)$/.test(rule.source) || !/^[A-Za-z0-9._:/-]{1,200}$/.test(rule.target)) return "映射源只允许末尾通配符，目标必须是精确模型 ID";
    if (seen.has(rule.source) || rule.source === rule.target) return `模型条目冲突：${rule.source}`;
    seen.add(rule.source);
  }
  return "";
}

function ProfileEditor({ label, value, disabled, change }: { label: string; value: Profile; disabled: boolean; change: (next: Profile) => void }) {
  const updateWhitelist = (index: number, model: string) => change({ ...value, whitelist: value.whitelist.map((old, i) => i === index ? model : old) });
  return <div className="account-template-fields"><span className="muted">{!value.whitelist.length && !value.mappings.length ? "不限制模型" : `${value.whitelist.length + value.mappings.length} 项`}</span>
    <div className="account-template-heading"><h4>白名单</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, whitelist: [...value.whitelist, ""] })}><Plus size={14}/>添加</button></div>
    {value.whitelist.map((model, index) => <div className="account-template-row" key={`w-${index}`}><input aria-label={`${label}白名单 ${index + 1}`} value={model} disabled={disabled} onChange={(e) => updateWhitelist(index, e.target.value.trim())}/><button className="icon-button" aria-label={`删除${label}白名单 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, whitelist: value.whitelist.filter((_, i) => i !== index) })}><X size={16}/></button></div>)}
    <div className="account-template-heading"><h4>模型映射</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, mappings: [...value.mappings, { source: "", target: "" }] })}><Plus size={14}/>添加</button></div>
    {value.mappings.map((rule, index) => <div className="account-template-row mapping" key={`m-${index}`}><input aria-label={`${label}请求模型 ${index + 1}`} placeholder="请求模型" value={rule.source} disabled={disabled} onChange={(e) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, source: e.target.value.trim() } : old) })}/><ArrowRight size={14}/><input aria-label={`${label}转发模型 ${index + 1}`} placeholder="转发模型" value={rule.target} disabled={disabled} onChange={(e) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, target: e.target.value.trim() } : old) })}/><button className="icon-button" aria-label={`删除${label}映射 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, mappings: value.mappings.filter((_, i) => i !== index) })}><X size={16}/></button></div>)}
  </div>;
}


function policy(profile: Profile) { return !profile.whitelist.length && !profile.mappings.length ? "不限制模型" : [...profile.whitelist, ...profile.mappings.map(rule => `${rule.source} → ${rule.target}`)].join("、"); }

export default function AccountTemplates({ accounts, online, initialAccount, initialAccounts, close, report }: { accounts: Account[]; online: boolean; initialAccount?: Account | null; initialAccounts?: Account[]; close: () => void; report: (error: unknown) => void }) {
  const eligibleAccounts = accounts.filter(a => a.platform === "openai" && ["oauth", "apikey"].includes(a.type));
  const [saved, setSaved] = useState<Config | null>(null), [drafts, setDrafts] = useState<Record<string, Draft>>({});
  const [accountId, setAccountId] = useState(initialAccount?.id ?? initialAccounts?.[0]?.id ?? eligibleAccounts[0]?.id ?? 0);
  const targetIds = initialAccounts ? [...new Set(initialAccounts.map(a => a.id))] : accountId ? [accountId] : [];
  const targetKey = targetIds.join(",");
  const [accountConfigs, setAccountConfigs] = useState<Record<number, NonNullable<AccountConfig["account"]>>>({});
  const [accountErrors, setAccountErrors] = useState<Record<number, string>>({});
  const [selected, setSelected] = useState(""), [creating, setCreating] = useState<Draft | null>(null), [deleteId, setDeleteId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState(""), [batch, setBatch] = useState<Batch | null>(null), [batchError, setBatchError] = useState("");
  const configGeneration = useRef(0), accountGeneration = useRef(0), savedRef = useRef<Config | null>(null);
  const pendingRequest = useRef<{ signature: string; request_id: string } | null>(null);
  useBackAction(true, () => deleteId ? setDeleteId(null) : close());

  function accept(value: Config, changed?: string) {
    const previous = savedRef.current;
    setDrafts(current => {
      const next = draftsOf(value), old = previous ? draftsOf(previous) : {};
      for (const id of value.template_order) if (id !== changed && current[id] && old[id] && !same(current[id], old[id])) next[id] = current[id];
      return next;
    });
    savedRef.current = value; setSaved(value);
    setSelected(current => value.templates[current] ? current : value.template_order[0] ?? "");
  }
  async function read() {
    const current = ++configGeneration.current; setBusy(true); setError("");
    try { const value = await api<Config>("GET", "/account-templates"); if (current === configGeneration.current) accept(value); }
    catch (e) { if (current === configGeneration.current) setError(e instanceof Error ? e.message : "模板读取失败"); }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function readAccounts() {
    const current = ++accountGeneration.current;
    setAccountConfigs({}); setAccountErrors({});
    const results: PromiseSettledResult<AccountConfig>[] = [];
    for (let offset = 0; offset < targetIds.length; offset += 4) {
      results.push(...await Promise.allSettled(targetIds.slice(offset, offset + 4).map(id => api<AccountConfig>("GET", `/account-templates?account_id=${id}`))));
      if (current !== accountGeneration.current) return;
    }
    if (current !== accountGeneration.current) return;
    const next: typeof accountConfigs = {}, failures: typeof accountErrors = {};
    results.forEach((result, index) => {
      const id = targetIds[index];
      if (result.status === "fulfilled" && result.value.account) next[id] = result.value.account;
      else failures[id] = result.status === "rejected" && result.reason instanceof Error ? result.reason.message : "账号配置读取失败";
    });
    setAccountConfigs(next); setAccountErrors(failures);
  }
  useEffect(() => { if (online) void read(); return () => { ++configGeneration.current; }; }, [online]);
  useEffect(() => { if (online) void readAccounts(); return () => { ++accountGeneration.current; }; }, [targetKey, online]);
  function update(id: string, change: Partial<Draft>) { setDrafts(current => ({ ...current, [id]: { ...current[id], ...change } })); }
  const dirty = (id: string) => !!saved && !same(drafts[id], { name: saved.template_meta[id]?.name, ...saved.templates[id] });
  const dirtyAny = !!creating || Object.keys(drafts).some(dirty);
  const selectedProfile = saved?.templates[selected];
  const ready = targetIds.length > 0 && targetIds.every(id => !!accountConfigs[id]) && targetIds.some(id => accountConfigs[id].eligible && !accountConfigs[id].passthrough);

  async function mutate(method: "POST" | "PUT" | "DELETE", id?: string) {
    if (!saved || busy || !online) return;
    const value = id ? drafts[id] : creating;
    if (method !== "DELETE") {
      if (!value?.name.trim() || value.name.trim().length > 40) { setError("模板名称需为 1–40 个字符"); return; }
      const invalid = validate(value); if (invalid) { setError(invalid); return; }
    }
    const current = configGeneration.current; setBusy(true); setError("");
    try {
      const result = await api<Config>(method, id ? `/account-templates/${id}` : "/account-templates", {
        expected_version: saved.version, ...(method === "DELETE" ? {} : { name: value!.name.trim(), ...profileOf(value!) }),
      });
      if (current !== configGeneration.current) return;
      accept(result, id);
      if (method === "POST") { setSelected(result.template_order.find(key => !saved.templates[key]) ?? ""); setCreating(null); }
      if (method === "DELETE") setDeleteId(null);
    } catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板保存失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }

  function acceptBatch(value: Batch) {
    setBatch(value);
    const ids = new Set(value.items.map(item => item.id));
    setOperations([...value.items, ...getOperations().filter(item => !ids.has(item.id))]);
  }
  async function readBatch(id: string, generation = configGeneration.current) {
    const epoch = operationEpoch();
    try {
      const value = await api<Batch>("GET", `/account-operations?batch_id=${encodeURIComponent(id)}`);
      if (generation !== configGeneration.current || epoch !== operationEpoch()) return;
      acceptBatch(value); setBatchError("");
    } catch (e) { if (generation === configGeneration.current && epoch === operationEpoch()) setBatchError(e instanceof Error ? e.message : "批次读取失败"); }
  }
  useEffect(() => {
    if (!online || !batch || !running(batch) || batchError) return;
    const timer = setTimeout(() => void readBatch(batch.batch_id), 2000);
    return () => clearTimeout(timer);
  }, [online, batch, batchError]);
  async function apply() {
    if (!saved || !selectedProfile || dirty(selected) || !ready || busy || !online || running(batch)) return;
    const current = configGeneration.current, epoch = operationEpoch(); setBusy(true); setError("");
    const payload = { template_id: selected, template_version: saved.template_versions?.[selected] ?? saved.version,
      accounts: targetIds.map(id => ({ account_id: id, expected_version: accountConfigs[id].version })), client_id: clientId().replaceAll("-", "") };
    const signature = JSON.stringify(payload);
    if (pendingRequest.current?.signature !== signature) pendingRequest.current = { signature, request_id: crypto.randomUUID().replaceAll("-", "") };
    try {
      const result = await api<Batch>("POST", "/account-templates/apply", { ...payload, request_id: pendingRequest.current.request_id });
      if (current !== configGeneration.current || epoch !== operationEpoch()) return;
      acceptBatch(result); setBatchError("");
      await readBatch(result.batch_id, current);
      if (current === configGeneration.current && epoch === operationEpoch()) void command("refresh");
    } catch (e) { if (current === configGeneration.current && epoch === operationEpoch()) { setError(e instanceof Error ? e.message : "模板应用失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function refreshPreview() {
    // A new preview is an explicit retry; a failed transport otherwise retains its idempotency ID.
    pendingRequest.current = null; setBatch(null); setBatchError("");
    await Promise.allSettled([read(), readAccounts()]);
  }
  function targetName(id: number) { return accounts.find(a => a.id === id)?.name ?? initialAccounts?.find(a => a.id === id)?.name ?? initialAccount?.name ?? "账号"; }
  const disabled = busy || !online;
  const failure = error || Object.values(accountErrors)[0];

  return <div className="modal-backdrop" onClick={close}><section className="account-templates-dialog" role="dialog" aria-modal="true" aria-label="账号模板" onClick={e => e.stopPropagation()}>
    <header><h2>账号模板</h2><button className="icon-button" aria-label="关闭账号模板" onClick={close}><X size={19}/></button></header>
    {!saved ? <div className="dialog-loading">{busy && <LoaderCircle className="spin"/>}</div> : <>
      <div className="account-template-grid">{saved.template_order.map(id => {
        const value = drafts[id]; if (!value) return null;
        return <section className="account-template-card" key={id}>
          <header><input className="account-template-name" aria-label={`模板名称 ${id}`} maxLength={40} value={value.name} disabled={disabled} onChange={e => update(id, { name: e.target.value })}/><div className="account-template-tools">
            <button className="icon-button" title="保存模板" aria-label={`保存${value.name}模板`} disabled={disabled || (!dirty(id) && saved.configured)} onClick={() => void mutate("PUT", id)}><Save size={17}/></button>
            <button className="icon-button danger-text" title="删除模板" aria-label={`删除${value.name}模板`} disabled={disabled} onClick={() => setDeleteId(id)}><Trash2 size={17}/></button>
          </div></header>
          {deleteId === id && <div className="account-template-delete" role="alert"><span>删除“{value.name}”？</span><button className="danger-text" disabled={disabled} onClick={() => void mutate("DELETE", id)}>确认删除</button><button disabled={busy} onClick={() => setDeleteId(null)}>取消</button></div>}
          <ProfileEditor label={value.name} value={value} disabled={disabled} change={next => update(id, next)}/>
        </section>;
      })}
      {creating && <section className="account-template-card"><header><input className="account-template-name" aria-label="新模板名称" placeholder="模板名称" maxLength={40} value={creating.name} disabled={disabled} onChange={e => setCreating({ ...creating, name: e.target.value })}/><div className="account-template-tools"><button className="icon-button" title="保存模板" aria-label="保存新模板" disabled={disabled} onClick={() => void mutate("POST")}><Save size={17}/></button><button className="icon-button" title="取消新增" aria-label="取消新增模板" disabled={busy} onClick={() => setCreating(null)}><X size={17}/></button></div></header><ProfileEditor label={creating.name || "新模板"} value={creating} disabled={disabled} change={next => setCreating({ ...creating, ...next })}/></section>}
      </div>
      <div className="account-template-actions"><button disabled={!dirtyAny || disabled} onClick={() => { setDrafts(draftsOf(saved)); setCreating(null); setDeleteId(null); setError(""); }}>放弃修改</button><button disabled={disabled || saved.template_order.length >= 32 || !!creating} onClick={() => setCreating({ name: "", ...emptyProfile() })}><Plus size={15}/>新增模板</button></div>
      <section className="account-template-apply"><div className="section-heading"><h3>应用到账号</h3><button className="icon-button" title="刷新预览" aria-label="刷新模板应用预览" disabled={disabled || running(batch)} onClick={() => void refreshPreview()}><RefreshCw size={15}/></button></div>
        <select aria-label="选择应用账号" value={initialAccounts ? "batch" : accountId} disabled={disabled || !!initialAccount || !!initialAccounts || running(batch)} onChange={e => { setAccountId(Number(e.target.value)); setBatch(null); }}>
          {initialAccounts ? <option value="batch">已选择 {targetIds.length} 个账号</option> : <><option value={0}>选择 OpenAI OAuth 或 Key 账号</option>{eligibleAccounts.map(a => <option key={a.id} value={a.id}>{a.name} #{a.id}</option>)}</>}
        </select>
        <div className="account-template-apply-row"><select aria-label="选择账号模板" value={selected} disabled={disabled || running(batch) || !saved.template_order.length} onChange={e => { setSelected(e.target.value); setBatch(null); }}>{saved.template_order.map(id => <option key={id} value={id}>{saved.template_meta[id].name}</option>)}</select><button className="primary" disabled={disabled || !ready || !saved.configured || !selectedProfile || dirty(selected) || running(batch)} onClick={() => void apply()}>应用模板</button></div>
        <div className="account-template-previews">{targetIds.map(id => {
          const value = accountConfigs[id], job = batch?.items.find(item => item.account_id === id);
          return <article key={id} className="account-template-preview"><header><strong>{targetName(id)} <span className="muted">#{id}</span></strong>{job && <span>{statusNames[job.status] ?? job.status}</span>}</header>
            {!value ? <span className="muted">{accountErrors[id] || "读取中…"}</span> : <>
              {selectedProfile && <p className="muted">当前：{policy(value.config)}<br/>目标：{policy(selectedProfile)}{same(value.config, selectedProfile) && <><br/>无变化</>}</p>}
              {(!value.eligible || value.passthrough) && <p className="bad-text">{value.reason || (value.passthrough ? "透传模式会绕过模型限制，不能自动修改。" : "此账号不支持独立应用模板。")}</p>}
            </>}{job?.reason && <p className={job.status === "failed" || job.status === "needs_confirmation" ? "bad-text" : "muted"}>{job.reason}</p>}
          </article>;
        })}</div>
        {batchError && <div role="alert"><span className="bad-text">{batchError}</span><button disabled={disabled} onClick={() => batch && void readBatch(batch.batch_id)}>重试读取</button></div>}
      </section>
    </>}
    {failure && <div><p className="bad-text" role="alert">{failure}</p><button disabled={disabled} onClick={() => { void read(); void readAccounts(); }}>重新读取</button></div>}
  </section></div>;
}
