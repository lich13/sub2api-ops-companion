import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowRight, LoaderCircle, Plus, RefreshCw, X } from "lucide-react";
import { api } from "./bridge";
import { useBackAction } from "./mobile";
import "./account-profiles.css";

type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
type Config = { version: string; configured: boolean; normal: Profile; degraded: Profile; latest_job?: Job | null };
type Item = { account_id: number; name: string; marked: boolean; before: Profile; after: Profile; status: string; error?: string; unrestricted: boolean };
type Preview = { version: string; items: Item[] };
type Job = { id: string; items: Item[] };
const labels: Record<string, string> = { queued: "待应用", writing: "正在核对", applied: "已应用", unchanged: "无变化", conflict: "冲突", failed: "失败" };
const active = (job: Job | null) => job?.items.some((item) => ["queued", "writing"].includes(item.status)) ?? false;

function validate(profile: Profile) {
  const seen = new Set<string>();
  for (const model of profile.whitelist) {
    if (!/^[A-Za-z0-9._:/-]{1,200}$/.test(model)) return "白名单必须是精确模型 ID";
    if (seen.has(model)) return `重复的模型：${model}`;
    seen.add(model);
  }
  for (const { source, target } of profile.mappings) {
    if (!/^(?:[A-Za-z0-9._:/-]{1,199}\*?|\*)$/.test(source) || !/^[A-Za-z0-9._:/-]{1,200}$/.test(target)) return "映射源只允许末尾通配符，目标必须是精确模型 ID";
    if (seen.has(source) || source === target) return `模型条目冲突：${source}`;
    seen.add(source);
  }
  return "";
}

function ModelInput({ label, value, placeholder, disabled, change }: { label: string; value: string; placeholder?: string; disabled: boolean; change: (value: string) => void }) {
  const field = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => {
    const resize = () => { if (field.current) { field.current.style.height = "0px"; field.current.style.height = `${field.current.scrollHeight + field.current.offsetHeight - field.current.clientHeight}px`; } };
    resize(); window.addEventListener("resize", resize);
    return () => window.removeEventListener("resize", resize);
  }, [value]);
  return <textarea ref={field} rows={1} aria-label={label} value={value} placeholder={placeholder} disabled={disabled} spellCheck={false} onChange={(e) => change(e.target.value.replace(/[\r\n]/g, "").trim())}/>;
}

function ProfileEditor({ title, value, change, disabled }: { title: string; value: Profile; change: (value: Profile) => void; disabled: boolean }) {
  return <section className="account-profile-card"><h3>{title}</h3>
    <div className="account-profile-label"><h4>白名单</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, whitelist: [...value.whitelist, ""] })}><Plus size={14}/>添加</button></div>
    {value.whitelist.map((model, index) => <div className="account-profile-row" key={index}>
      <ModelInput label={`${title}白名单 ${index + 1}`} value={model} disabled={disabled} change={(model) => change({ ...value, whitelist: value.whitelist.map((old, i) => i === index ? model : old) })}/>
      <button className="icon-button" aria-label={`删除${title}白名单 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, whitelist: value.whitelist.filter((_, i) => i !== index) })}><X size={16}/></button>
    </div>)}
    <div className="account-profile-label"><h4>模型映射</h4><button className="text-button" disabled={disabled} onClick={() => change({ ...value, mappings: [...value.mappings, { source: "", target: "" }] })}><Plus size={14}/>添加</button></div>
    {value.mappings.map((rule, index) => <div className="account-profile-row mapping" key={index}>
      <ModelInput label={`${title}请求模型 ${index + 1}`} placeholder="请求模型" disabled={disabled} value={rule.source} change={(source) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, source } : old) })}/><ArrowRight size={14}/>
      <ModelInput label={`${title}转发模型 ${index + 1}`} placeholder="转发模型" disabled={disabled} value={rule.target} change={(target) => change({ ...value, mappings: value.mappings.map((old, i) => i === index ? { ...old, target } : old) })}/>
      <button className="icon-button" aria-label={`删除${title}映射 ${index + 1}`} disabled={disabled} onClick={() => change({ ...value, mappings: value.mappings.filter((_, i) => i !== index) })}><X size={16}/></button>
    </div>)}
  </section>;
}

function Policy({ value }: { value: Profile }) {
  return <div className="account-profile-policy">{!value.whitelist.length && !value.mappings.length ? <strong>不限制模型</strong> : <>{value.whitelist.map((model) => <span key={model}>{model}</span>)}{value.mappings.map((rule) => <span key={rule.source}>{rule.source} → {rule.target}</span>)}</>}</div>;
}

export default function AccountModelProfiles({ online }: { online: boolean }) {
  const [saved, setSaved] = useState<Config | null>(null), [draft, setDraft] = useState<Config | null>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null), [job, setJob] = useState<Job | null>(null);
  const generation = useRef(0);
  useBackAction(!!preview, () => setPreview(null));
  async function read() {
    const id = ++generation.current;
    setBusy(true); setError("");
    try { const value = await api<Config>("GET", "/account-model-profiles"); if (id === generation.current) { setSaved(value); setDraft(structuredClone(value)); setJob(value.latest_job ?? null); } }
    catch (e) { if (id === generation.current) setError(e instanceof Error ? e.message : "模板读取失败"); }
    finally { if (id === generation.current) setBusy(false); }
  }
  useEffect(() => { if (online) void read(); return () => { ++generation.current; }; }, [online]);
  const running = active(job);
  useEffect(() => {
    if (!job || !running || !online) return;
    let alive = true, timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try { const value = await api<Job>("GET", `/account-model-profiles/jobs/${job.id}`); if (alive) setJob(value); }
      catch (e) { if (alive) setError(e instanceof Error ? e.message : "任务读取失败"); }
      if (alive) timer = setTimeout(() => void poll(), 2000);
    };
    timer = setTimeout(() => void poll(), 500);
    return () => { alive = false; clearTimeout(timer); };
  }, [job?.id, running, online]);
  const dirty = !!draft && JSON.stringify(draft) !== JSON.stringify(saved);
  async function action(kind: "save" | "preview" | "apply") {
    if (!draft || !saved || busy || !online) return;
    const invalid = validate(draft.normal) || validate(draft.degraded);
    if (invalid) { setError(invalid); return; }
    const id = generation.current;
    setBusy(true); setError("");
    try {
      if (kind === "save") {
        const value = await api<Config>("PUT", "/account-model-profiles", { expected_version: saved.version, normal: draft.normal, degraded: draft.degraded });
        if (id === generation.current) { setSaved(value); setDraft(structuredClone(value)); }
      } else if (kind === "preview") {
        const value = await api<Preview>("GET", "/account-model-profiles/preview");
        if (id === generation.current) setPreview(value);
      } else if (preview) {
        const value = await api<Job>("POST", "/account-model-profiles/apply", { preview_version: preview.version, request_id: crypto.randomUUID() });
        if (id === generation.current) { setJob(value); setPreview(null); }
      }
    } catch (e) { if (id === generation.current) setError(e instanceof Error ? e.message : "操作失败"); }
    finally { if (id === generation.current) setBusy(false); }
  }
  return <section className="account-model-profiles feature-models"><div className="section-heading"><h2>账号模型配置</h2><button className="icon-button" aria-label="重新读取账号模型模板" disabled={busy || dirty || !online} onClick={() => void read()}><RefreshCw size={16}/></button></div>
    {draft ? <><div className="account-profiles-grid"><ProfileEditor title="未标记降智" value={draft.normal} change={(normal) => setDraft({ ...draft, normal })} disabled={busy || !online}/><ProfileEditor title="已标记降智" value={draft.degraded} change={(degraded) => setDraft({ ...draft, degraded })} disabled={busy || !online}/></div>
      <footer className="account-profile-actions">{(dirty || !saved?.configured) && <><button disabled={busy || !online} onClick={() => { setDraft(structuredClone(saved)); setError(""); }}>放弃</button><button className="primary" disabled={busy || !online} onClick={() => void action("save")}>保存模板</button></>}<button disabled={busy || dirty || !online || !saved?.configured || running} onClick={() => void action("preview")}>{busy && <LoaderCircle className="spin" size={15}/>}一键应用</button></footer>
    </> : busy ? <LoaderCircle className="spin" size={18}/> : <button disabled={!online} onClick={() => void read()}>重试</button>}
    {error && <p className="bad-text" role="alert">{error}</p>}
    {job && <div className="account-profile-results" aria-live="polite">{job.items.filter((item) => item.status !== "unchanged").map((item) => <div key={item.account_id}><strong>{item.name} #{item.account_id}</strong><span className={item.error ? "bad-text" : "muted"}>{item.error || labels[item.status]}</span></div>)}</div>}
    {preview && <div className="modal-backdrop" onClick={() => !busy && setPreview(null)}><section className="account-profile-preview" role="dialog" aria-modal="true" aria-label="账号模型配置预览" onClick={(e) => e.stopPropagation()}><header><h2>应用账号模型配置</h2><button className="icon-button" aria-label="关闭配置预览" disabled={busy} onClick={() => setPreview(null)}><X size={18}/></button></header>
      <div className="account-profile-diffs">{preview.items.map((item) => <article key={item.account_id}><div className="section-heading"><strong>{item.name} #{item.account_id}</strong><span>{item.marked ? "已标记降智" : "未标记降智"} · {labels[item.status]}</span></div><div className="account-profile-diff"><div><h4>当前</h4><Policy value={item.before}/></div><ArrowRight size={16}/><div><h4>应用后</h4><Policy value={item.after}/></div></div>{item.error && <p className="bad-text">{item.error}</p>}</article>)}</div>
      {error && <p className="bad-text" role="alert">{error}</p>}<footer><button disabled={busy} onClick={() => setPreview(null)}>取消</button><button className="primary" disabled={busy || !online || !preview.items.some((item) => item.status === "queued")} onClick={() => void action("apply")}>应用变更 {preview.items.filter((item) => item.status === "queued").length}</button></footer>
    </section></div>}
  </section>;
}
