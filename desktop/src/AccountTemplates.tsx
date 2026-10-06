import { useEffect, useRef, useState } from "react";
import { ArrowRight, LoaderCircle, Plus, RefreshCw, X } from "lucide-react";
import { api, command } from "./bridge";
import { accountOperation } from "./accountOperations";
import { useBackAction } from "./mobile";
import type { Account } from "./types";

type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
type TemplateId = "full" | "degraded" | "takeover";
type Config = { version: string; configured: boolean; templates: Record<TemplateId, Profile> };
type AccountConfig = { account?: { id: number; eligible: boolean; passthrough: boolean; version: string; config: Profile } };
const labels: Record<TemplateId, string> = { full: "满血", degraded: "降智", takeover: "降智接管" };

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
function ProfileEditor({ id, value, disabled, change }: { id: TemplateId; value: Profile; disabled: boolean; change: (next: Profile) => void }) {
  const updateWhitelist = (index: number, model: string) => change({ ...value, whitelist: value.whitelist.map((old, i) => i === index ? model : old) });
  return <section className="account-template-card"><header><h3>{labels[id]}</h3><span className="muted">{!value.whitelist.length && !value.mappings.length ? "不限制模型" : `${value.whitelist.length + value.mappings.length} 项`}</span></header>
    <div className="account-template-heading"><h4>白名单</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, whitelist: [...value.whitelist, ""] })}><Plus size={14}/>添加</button></div>
    {value.whitelist.map((model, index) => <div className="account-template-row" key={`w-${index}`}><input aria-label={`${labels[id]}白名单 ${index + 1}`} value={model} disabled={disabled} onChange={(e) => updateWhitelist(index, e.target.value.trim())}/><button className="icon-button" aria-label={`删除${labels[id]}白名单 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, whitelist: value.whitelist.filter((_, i) => i !== index) })}><X size={16}/></button></div>)}
    <div className="account-template-heading"><h4>模型映射</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, mappings: [...value.mappings, { source: "", target: "" }] })}><Plus size={14}/>添加</button></div>
    {value.mappings.map((rule, index) => <div className="account-template-row mapping" key={`m-${index}`}><input aria-label={`${labels[id]}请求模型 ${index + 1}`} placeholder="请求模型" value={rule.source} disabled={disabled} onChange={(e) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, source: e.target.value.trim() } : old) })}/><ArrowRight size={14}/><input aria-label={`${labels[id]}转发模型 ${index + 1}`} placeholder="转发模型" value={rule.target} disabled={disabled} onChange={(e) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, target: e.target.value.trim() } : old) })}/><button className="icon-button" aria-label={`删除${labels[id]}映射 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, mappings: value.mappings.filter((_, i) => i !== index) })}><X size={16}/></button></div>)}
  </section>;
}
function policy(profile: Profile) { return !profile.whitelist.length && !profile.mappings.length ? "不限制模型" : [...profile.whitelist, ...profile.mappings.map((rule) => `${rule.source} → ${rule.target}`)].join("、"); }
export default function AccountTemplates({ accounts, online, initialAccount, close, report }: { accounts: Account[]; online: boolean; initialAccount?: Account | null; close: () => void; report: (error: unknown) => void }) {
  const [saved, setSaved] = useState<Config | null>(null), [draft, setDraft] = useState<Config | null>(null);
  const [accountId, setAccountId] = useState<number>(initialAccount?.id ?? accounts.find((a) => a.platform === "openai" && ["oauth", "apikey"].includes(a.type))?.id ?? 0);
  const [accountConfig, setAccountConfig] = useState<AccountConfig | null>(null), [selected, setSelected] = useState<TemplateId>("full");
  const [busy, setBusy] = useState(false), [error, setError] = useState("");
  const configGeneration = useRef(0), accountGeneration = useRef(0);
  useBackAction(true, close);
  const eligibleAccounts = accounts.filter((a) => a.platform === "openai" && ["oauth", "apikey"].includes(a.type));
  async function read() {
    const current = ++configGeneration.current; setBusy(true); setError("");
    try {
      const value = await api<Config>("GET", "/account-templates");
      if (current === configGeneration.current) { setSaved(value); setDraft(structuredClone(value)); }
    } catch (e) { if (current === configGeneration.current) setError(e instanceof Error ? e.message : "模板读取失败"); }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function readAccount(id: number) {
    if (!id) { setAccountConfig(null); return; }
    const current = ++accountGeneration.current; setAccountConfig(null); setError("");
    try { const value = await api<AccountConfig>("GET", `/account-templates?account_id=${id}`); if (current === accountGeneration.current) setAccountConfig(value); }
    catch (e) { if (current === accountGeneration.current) setError(e instanceof Error ? e.message : "账号配置读取失败"); }
  }
  useEffect(() => { if (online) { void read(); } return () => { ++configGeneration.current; ++accountGeneration.current; }; }, [online]);
  useEffect(() => { if (online) void readAccount(accountId); }, [accountId, online]);
  const dirty = !!saved && !!draft && JSON.stringify(saved) !== JSON.stringify(draft);
  async function save() {
    if (!saved || !draft || busy || !online) return;
    for (const id of ["full", "degraded", "takeover"] as TemplateId[]) { const invalid = validate(draft.templates[id]); if (invalid) { setError(`${labels[id]}：${invalid}`); return; } }
    const current = configGeneration.current; setBusy(true); setError("");
    try { const value = await api<Config>("PUT", "/account-templates", { expected_version: saved.version, ...draft.templates }); if (current === configGeneration.current) { setSaved(value); setDraft(structuredClone(value)); } }
    catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板保存失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  async function apply() {
    if (dirty || !accountConfig?.account || !saved || busy || !online || !accountConfig.account.eligible || accountConfig.account.passthrough) return;
    const current = configGeneration.current;
    setBusy(true); setError("");
    try {
      const target = accounts.find((item) => item.id === accountId);
      if (!target) throw new Error("账号已不在当前连接中");
      await accountOperation(target, "account_template", { template_id: selected, template_version: saved.version }, accountConfig.account.version);
      if (current === configGeneration.current) { await readAccount(accountId); await command("refresh"); }
    }
    catch (e) { if (current === configGeneration.current) { setError(e instanceof Error ? e.message : "模板应用失败"); report(e); } }
    finally { if (current === configGeneration.current) setBusy(false); }
  }
  return <div className="modal-backdrop" onClick={close}><section className="account-templates-dialog" role="dialog" aria-modal="true" aria-label="账号模板" onClick={(e) => e.stopPropagation()}>
    <header><div><h2>账号模板</h2></div><button className="icon-button" aria-label="关闭账号模板" onClick={close}><X size={19}/></button></header>
    {!draft ? <div className="dialog-loading">{busy ? <LoaderCircle className="spin"/> : <button onClick={() => void read()}>重试</button>}</div> : <>
      <div className="account-template-grid">{(["full", "degraded", "takeover"] as TemplateId[]).map((id) => <ProfileEditor key={id} id={id} value={draft.templates[id]} disabled={busy || !online} change={(next) => setDraft({ ...draft, templates: { ...draft.templates, [id]: next } })}/>)}</div>
      {!saved?.configured && <p className="hint">尚未配置模板，保存后即可维护三套模板。</p>}
      <div className="account-template-actions"><button disabled={!dirty || busy || !online} onClick={() => { if (saved) setDraft(structuredClone(saved)); setError(""); }}>放弃修改</button><button className="primary" disabled={(!dirty && !!saved?.configured) || busy || !online} onClick={() => void save()}>{busy && <LoaderCircle size={15} className="spin"/>}保存模板</button></div>
      <section className="account-template-apply"><div className="section-heading"><h3>应用到账号</h3><RefreshCw size={15} className={busy ? "spin" : ""}/></div><select aria-label="选择应用账号" value={accountId} disabled={busy || !online || !!initialAccount} onChange={(e) => { const id = Number(e.target.value); setAccountId(id); }}><option value={0}>选择 OpenAI OAuth 或 Key 账号</option>{eligibleAccounts.map((a) => <option key={a.id} value={a.id}>{a.name} #{a.id}</option>)}</select><div className="account-template-apply-row"><select aria-label="选择账号模板" value={selected} disabled={busy || !online} onChange={(e) => setSelected(e.target.value as TemplateId)}>{(["full", "degraded", "takeover"] as TemplateId[]).map((id) => <option key={id} value={id}>{labels[id]}</option>)}</select><button className="primary" disabled={dirty || !accountConfig?.account?.eligible || !!accountConfig.account.passthrough || busy || !online || !saved?.configured} onClick={() => void apply()}>应用模板</button></div>{accountConfig?.account && <p className="muted">当前：{policy(accountConfig.account.config)}<br/>目标：{policy(saved!.templates[selected])}</p>}{accountConfig?.account && !accountConfig.account.eligible && <p className="bad-text">此账号不支持独立应用模板。</p>}{accountConfig?.account?.passthrough && <p className="bad-text">透传模式会绕过模型限制，不能自动修改。</p>}</section>
    </>}
    {error && <div><p className="bad-text" role="alert">{error}</p><button disabled={busy || !online} onClick={() => { void read(); void readAccount(accountId); }}>重新读取</button></div>}
  </section></div>;
}
