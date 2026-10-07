import { useEffect, useRef, useState } from "react";
import { ArrowRight, LoaderCircle, Plus, RefreshCw, X } from "lucide-react";
import { api, command } from "./bridge";
import { accountOperation } from "./accountOperations";
import { useBackAction } from "./mobile";
import type { Account } from "./types";

type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
type BuiltinTemplateId = "full" | "degraded" | "takeover";
type CustomTemplate = Profile & { id: string; name: string };
type Config = { version: string; configured: boolean; templates: Record<BuiltinTemplateId, Profile>; custom_templates?: CustomTemplate[] };
type AccountConfig = { account?: { id: number; eligible: boolean; passthrough: boolean; version: string; config: Profile } };
type CustomDraft = { name: string; whitelist: string[]; mappings: { source: string; target: string }[] };
const builtinIds: BuiltinTemplateId[] = ["full", "degraded", "takeover"];
const labels: Record<BuiltinTemplateId, string> = { full: "满血", degraded: "降智", takeover: "降智接管" };
const emptyProfile = (): Profile => ({ whitelist: [], mappings: [] });

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
  return <section className="account-template-card"><header><h3>{label}</h3><span className="muted">{!value.whitelist.length && !value.mappings.length ? "不限制模型" : `${value.whitelist.length + value.mappings.length} 项`}</span></header>
    <div className="account-template-heading"><h4>白名单</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, whitelist: [...value.whitelist, ""] })}><Plus size={14}/>添加</button></div>
    {value.whitelist.map((model, index) => <div className="account-template-row" key={`w-${index}`}><input aria-label={`${label}白名单 ${index + 1}`} value={model} disabled={disabled} onChange={(e) => updateWhitelist(index, e.target.value.trim())}/><button className="icon-button" aria-label={`删除${label}白名单 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, whitelist: value.whitelist.filter((_, i) => i !== index) })}><X size={16}/></button></div>)}
    <div className="account-template-heading"><h4>模型映射</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, mappings: [...value.mappings, { source: "", target: "" }] })}><Plus size={14}/>添加</button></div>
    {value.mappings.map((rule, index) => <div className="account-template-row mapping" key={`m-${index}`}><input aria-label={`${label}请求模型 ${index + 1}`} placeholder="请求模型" value={rule.source} disabled={disabled} onChange={(e) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, source: e.target.value.trim() } : old) })}/><ArrowRight size={14}/><input aria-label={`${label}转发模型 ${index + 1}`} placeholder="转发模型" value={rule.target} disabled={disabled} onChange={(e) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, target: e.target.value.trim() } : old) })}/><button className="icon-button" aria-label={`删除${label}映射 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, mappings: value.mappings.filter((_, i) => i !== index) })}><X size={16}/></button></div>)}
  </section>;
}

function policy(profile: Profile) { return !profile.whitelist.length && !profile.mappings.length ? "不限制模型" : [...profile.whitelist, ...profile.mappings.map((rule) => `${rule.source} → ${rule.target}`)].join("、"); }
function customProfile(value: CustomDraft): Profile { return { whitelist: value.whitelist, mappings: value.mappings }; }

export default function AccountTemplates({ accounts, online, initialAccount, close, report }: { accounts: Account[]; online: boolean; initialAccount?: Account | null; close: () => void; report: (error: unknown) => void }) {
  const [saved, setSaved] = useState<Config | null>(null), [draft, setDraft] = useState<Config | null>(null);
  const [accountId, setAccountId] = useState<number>(initialAccount?.id ?? accounts.find((a) => a.platform === "openai" && ["oauth", "apikey"].includes(a.type))?.id ?? 0);
  const [accountConfig, setAccountConfig] = useState<AccountConfig | null>(null), [selected, setSelected] = useState<string>("full");
  const [creating, setCreating] = useState<CustomDraft | null>(null), [deleteId, setDeleteId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState("");
  const configGeneration = useRef(0), accountGeneration = useRef(0);
  useBackAction(true, close);
  const eligibleAccounts = accounts.filter((a) => a.platform === "openai" && ["oauth", "apikey"].includes(a.type));

  async function read() {
    const current = ++configGeneration.current; setBusy(true); setError("");
    try {
      const value = await api<Config>("GET", "/account-templates");
      if (current === configGeneration.current) {
        value.custom_templates ??= [];
        setSaved(value); setDraft(structuredClone(value));
        if (![...builtinIds, ...value.custom_templates.map((item) => item.id)].includes(selected)) setSelected("full");
      }
    } catch (e) { if (current === configGeneration.current) setError(e instanceof Error ? e.message : "模板读取失败"); }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function readAccount(id: number) {
    if (!id) { setAccountConfig(null); return; }
    const current = ++accountGeneration.current; setAccountConfig(null); setError("");
    try { const value = await api<AccountConfig>("GET", `/account-templates?account_id=${id}`); if (current === accountGeneration.current) setAccountConfig(value); }
    catch (e) { if (current === accountGeneration.current) setError(e instanceof Error ? e.message : "账号配置读取失败"); }
  }
  useEffect(() => { if (online) void read(); return () => { ++configGeneration.current; ++accountGeneration.current; }; }, [online]);
  useEffect(() => { if (online) void readAccount(accountId); }, [accountId, online]);
  const dirtyBuiltins = !!saved && !!draft && builtinIds.some((id) => JSON.stringify(saved.templates[id]) !== JSON.stringify(draft.templates[id]));
  const dirtyCustom = !!saved && !!draft && saved.custom_templates?.some((item) => JSON.stringify(item) !== JSON.stringify(draft.custom_templates?.find((candidate) => candidate.id === item.id)));
  const dirty = dirtyBuiltins || dirtyCustom || !!creating;
  const options = draft ? [...builtinIds.map((id) => ({ id, name: labels[id], profile: draft.templates[id] })), ...(draft.custom_templates ?? []).map((item) => ({ id: item.id, name: item.name, profile: item }))] : [];
  const selectedProfile = options.find((item) => item.id === selected)?.profile;

  function accept(value: Config, changed: string | "builtins") {
    value.custom_templates ??= [];
    setDraft((current) => {
      const next = structuredClone(value);
      if (!current || !saved) return next;
      if (changed !== "builtins") for (const id of builtinIds) {
        if (JSON.stringify(current.templates[id]) !== JSON.stringify(saved.templates[id])) next.templates[id] = current.templates[id];
      }
      next.custom_templates = next.custom_templates!.map((item) => {
        const old = saved.custom_templates?.find((entry) => entry.id === item.id);
        const edit = current.custom_templates?.find((entry) => entry.id === item.id);
        return item.id !== changed && old && edit && JSON.stringify(old) !== JSON.stringify(edit) ? edit : item;
      });
      return next;
    });
    setSaved(value);
  }
  async function saveBuiltins() {
    if (!saved || !draft || busy || !online) return;
    for (const id of builtinIds) { const invalid = validate(draft.templates[id]); if (invalid) { setError(`${labels[id]}：${invalid}`); return; } }
    const current = configGeneration.current; setBusy(true); setError("");
    try {
      const value = await api<Config>("PUT", "/account-templates", { expected_version: saved.version, full: draft.templates.full, degraded: draft.templates.degraded, takeover: draft.templates.takeover });
      if (current === configGeneration.current) { accept(value, "builtins"); }
    } catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板保存失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function createCustom() {
    if (!saved || !creating || busy || !online) return;
    const name = creating.name.trim();
    if (!name) { setError("请填写模板名称"); return; }
    const invalid = validate(customProfile(creating)); if (invalid) { setError(invalid); return; }
    const current = configGeneration.current; setBusy(true); setError("");
    try {
      const value = await api<Config>("POST", "/account-templates/custom", { expected_version: saved.version, name, ...customProfile(creating) });
      if (current === configGeneration.current) { accept(value, "created"); const latest = value.custom_templates?.at(-1); if (latest) setSelected(latest.id); setCreating(null); }
    } catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板创建失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function saveCustom(item: CustomTemplate) {
    if (!saved || busy || !online) return;
    const invalid = validate(item); if (invalid) { setError(`${item.name}：${invalid}`); return; }
    const current = configGeneration.current; setBusy(true); setError("");
    try {
      const value = await api<Config>("PUT", `/account-templates/custom/${item.id}`, { expected_version: saved.version, name: item.name.trim(), whitelist: item.whitelist, mappings: item.mappings });
      if (current === configGeneration.current) { accept(value, item.id); }
    } catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板保存失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function deleteCustom(id: string) {
    if (!saved || busy || !online) return;
    const current = configGeneration.current; setBusy(true); setError("");
    try {
      const value = await api<Config>("DELETE", `/account-templates/custom/${id}`, { expected_version: saved.version });
      if (current === configGeneration.current) { accept(value, id); setDeleteId(null); if (selected === id) setSelected("full"); }
    } catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板删除失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function apply() {
    if (dirty || !accountConfig?.account || !saved || busy || !online || !accountConfig.account.eligible || accountConfig.account.passthrough || !selectedProfile) return;
    const current = configGeneration.current; setBusy(true); setError("");
    try {
      const target = accounts.find((item) => item.id === accountId);
      if (!target) throw new Error("账号已不在当前连接中");
      await accountOperation(target, "account_template", { template_id: selected, template_version: saved.version }, accountConfig.account.version);
      if (current === configGeneration.current) { await readAccount(accountId); await command("refresh"); }
    }
    catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板应用失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  function updateCustom(id: string, change: (item: CustomTemplate) => CustomTemplate) {
    if (!draft) return;
    setDraft({ ...draft, custom_templates: draft.custom_templates?.map((item) => item.id === id ? change(item) : item) ?? [] });
  }
  function updateCreate(change: (value: CustomDraft) => CustomDraft) { setCreating((current) => current ? change(current) : current); }

  return <div className="modal-backdrop" onClick={close}><section className="account-templates-dialog" role="dialog" aria-modal="true" aria-label="账号模板" onClick={(e) => e.stopPropagation()}>
    <header><div><h2>账号模板</h2></div><button className="icon-button" aria-label="关闭账号模板" onClick={close}><X size={19}/></button></header>
    {!draft ? <div className="dialog-loading">{busy ? <LoaderCircle className="spin"/> : <button onClick={() => void read()}>重试</button>}</div> : <>
      <div className="account-template-grid">{builtinIds.map((id) => <ProfileEditor key={id} label={labels[id]} value={draft.templates[id]} disabled={busy || !online} change={(next) => setDraft({ ...draft, templates: { ...draft.templates, [id]: next } })}/>)}</div>
      <div className="account-template-grid account-template-custom-grid">{(draft.custom_templates ?? []).map((item) => <section className="account-template-custom" key={item.id}><div className="account-template-custom-heading"><label><span>模板名称</span><input aria-label="自定义模板名称" value={item.name} disabled={busy || !online} onChange={(e) => updateCustom(item.id, (value) => ({ ...value, name: e.target.value }))}/></label><div><button className="primary" disabled={busy || !online} onClick={() => void saveCustom(item)}>保存自定义模板</button>{deleteId === item.id ? <><button className="danger-text" disabled={busy || !online} onClick={() => void deleteCustom(item.id)}>确认删除</button><button disabled={busy} onClick={() => setDeleteId(null)}>取消</button></> : <button className="danger-text" disabled={busy || !online} onClick={() => setDeleteId(item.id)}>删除自定义模板</button>}</div></div><ProfileEditor label={item.name || "自定义模板"} value={item} disabled={busy || !online} change={(next) => updateCustom(item.id, (value) => ({ ...value, ...next }))}/></section>)}</div>
      {creating && <section className="account-template-custom"><div className="account-template-custom-heading"><label><span>模板名称</span><input aria-label="自定义模板名称" value={creating.name} disabled={busy || !online} onChange={(e) => updateCreate((value) => ({ ...value, name: e.target.value }))}/></label><div><button className="primary" disabled={busy || !online} onClick={() => void createCustom()}>{busy && <LoaderCircle size={15} className="spin"/>}保存自定义模板</button><button disabled={busy} onClick={() => setCreating(null)}>取消</button></div></div><ProfileEditor label={creating.name || "新模板"} value={customProfile(creating)} disabled={busy || !online} change={(next) => updateCreate((value) => ({ ...value, ...next }))}/></section>}
      <div className="account-template-actions"><button disabled={!dirty || busy || !online} onClick={() => { if (saved) setDraft(structuredClone(saved)); setCreating(null); setDeleteId(null); setError(""); }}>放弃修改</button><button className="primary" disabled={(!dirtyBuiltins && !!saved?.configured) || busy || !online} onClick={() => void saveBuiltins()}>{busy && <LoaderCircle size={15} className="spin"/>}保存模板</button><button disabled={busy || !online || (draft.custom_templates?.length ?? 0) >= 29 || !!creating} onClick={() => setCreating({ name: "", ...emptyProfile() })}><Plus size={15}/>新增模板</button></div>
      {!saved?.configured && <p className="hint">保存后即可应用模型配置。</p>}
      <section className="account-template-apply"><div className="section-heading"><h3>应用到账号</h3><RefreshCw size={15} className={busy ? "spin" : ""}/></div><select aria-label="选择应用账号" value={accountId} disabled={busy || !online || !!initialAccount} onChange={(e) => setAccountId(Number(e.target.value))}><option value={0}>选择 OpenAI OAuth 或 Key 账号</option>{eligibleAccounts.map((a) => <option key={a.id} value={a.id}>{a.name} #{a.id}</option>)}</select><div className="account-template-apply-row"><select aria-label="选择账号模板" value={selected} disabled={busy || !online} onChange={(e) => setSelected(e.target.value)}>{options.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select><button className="primary" disabled={dirty || !accountConfig?.account?.eligible || !!accountConfig.account.passthrough || busy || !online || !saved?.configured || !selectedProfile} onClick={() => void apply()}>应用模板</button></div>{accountConfig?.account && selectedProfile && <p className="muted">当前：{policy(accountConfig.account.config)}<br/>目标：{policy(selectedProfile)}</p>}{accountConfig?.account && !accountConfig.account.eligible && <p className="bad-text">此账号不支持独立应用模板。</p>}{accountConfig?.account?.passthrough && <p className="bad-text">透传模式会绕过模型限制，不能自动修改。</p>}</section>
    </>}
    {error && <div><p className="bad-text" role="alert">{error}</p><button disabled={busy || !online} onClick={() => { void read(); void readAccount(accountId); }}>重新读取</button></div>}
  </section></div>;
}
