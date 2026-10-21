import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, LoaderCircle, Minus, Plus, X } from "lucide-react";
import { command } from "./bridge";
import { followOperation, operationEpoch, submitOperation } from "./accountOperations";
import { operationLabel } from "./AccountSettingsDialog";
import { useBackAction } from "./mobile";
import { useDialogFocus } from "./dialogFocus";
import type { Account } from "./types";
import "./account-settings.css";

export type SchedulingField = "concurrency" | "load_factor";
const labels = { concurrency: "并发", load_factor: "负载因子" };
export function NumericEditor({ account, field, online, close, anchor }: { account: Account; field: SchedulingField; online: boolean; close: () => void; anchor: DOMRect }) {
  const initial = useRef({ value: account[field], version: account.operation_versions?.[field] });
  const [value, setValue] = useState(String(initial.current.value ?? "")), [automatic, setAutomatic] = useState(field === "load_factor" && initial.current.value == null);
  const [busy, setBusy] = useState(false), [error, setError] = useState(""), [notice, setNotice] = useState("");
  const alive = useRef(true), running = useRef(false), input = useRef<HTMLInputElement>(null);
  const dialog = useRef<HTMLFormElement>(null);
  useDialogFocus(dialog);
  const min = 1, max = field === "load_factor" ? 10000 : 2147483647;
  function edit(value: string) { setValue(value); setNotice(""); }
  useEffect(() => { alive.current = true; input.current?.focus(); input.current?.select(); return () => { alive.current = false; }; }, []);
  useBackAction(true, close);
  async function save() {
    if (running.current || !online) return;
    const number = Number(value);
    if (!automatic && (!/^\d+$/.test(value) || !Number.isSafeInteger(number) || number < min || number > max)) { setError(`${labels[field]}必须是 ${min}–${max} 的整数`); return; }
    running.current = true; setBusy(true); setError(""); setNotice("");
    const epoch = operationEpoch();
    try {
      const job = await submitOperation(account, field, { [field]: automatic ? null : number }, initial.current.version);
      if (alive.current) setNotice(operationLabel(job));
      const result = await followOperation<{ verified: boolean; field_version?: string }>(job, undefined, next => { if (alive.current) setNotice(operationLabel(next)); });
      if (epoch === operationEpoch()) void command("refresh");
      if (alive.current) {
        if (!result.verified) throw new Error("字段保存尚未确认，请在待办中核对");
        if (result.field_version) initial.current.version = result.field_version;
        setNotice("已确认生效");
      }
    } catch (e) { if (alive.current) { setNotice(""); setError(String(e).replace(/^Error: /, "")); } }
    finally { running.current = false; if (alive.current) setBusy(false); }
  }
  const width = Math.min(300, window.innerWidth - 24), left = Math.max(12, Math.min(anchor.left, window.innerWidth - width - 12));
  const top = Math.max(12, Math.min(anchor.bottom + 6, window.innerHeight - 270));
  return createPortal(<div className="numeric-editor-backdrop" onClick={close}>
    <form ref={dialog} className="numeric-editor" role="dialog" aria-modal="true" aria-label={`编辑${labels[field]}`} style={{ left, top, width, maxHeight: `calc(100dvh - ${top + 12}px)` }} onClick={e => e.stopPropagation()} onSubmit={e => { e.preventDefault(); void save(); }} onKeyDown={e => { if (e.key === "Escape") { e.stopPropagation(); close(); } }}>
      <header><strong>{labels[field]}</strong><button type="button" className="icon-button" aria-label="关闭参数编辑" onClick={close}><X size={16}/></button></header>
      <div className="numeric-input-row"><button type="button" aria-label={`减少${labels[field]}`} disabled={busy || automatic} onClick={() => edit(String(Math.max(min, (Number(value) || min) - 1)))}><Minus size={16}/></button>
        <input ref={input} type="text" inputMode="numeric" pattern="[0-9]*" aria-label={labels[field]} value={value} disabled={busy || automatic} onChange={e => edit(e.target.value)} autoComplete="off"/>
        <button type="button" aria-label={`增加${labels[field]}`} disabled={busy || automatic} onClick={() => edit(String(Math.min(max, (Number(value) || 0) + 1)))}><Plus size={16}/></button></div>
      {field === "load_factor" && <label className="numeric-auto"><input type="checkbox" checked={automatic} disabled={busy} onChange={e => { setAutomatic(e.target.checked); setNotice(""); if (!e.target.checked && !value) setValue(String(account.concurrency || 1)); }}/>自动（随并发）</label>}
      {error && <p role="alert" className="bad-text">{error}</p>}{notice && <p role="status">{notice}</p>}
      <footer><button type="button" onClick={close}>取消</button><button type="submit" className="primary" disabled={busy || notice === "已确认生效"}>{busy ? <LoaderCircle className="spin" size={15}/> : <Check size={15}/>}保存</button></footer>
    </form>
  </div>, document.body);
}
export default function SchedulingParameters({ account, online }: { account: Account; online: boolean; report?: (error: unknown) => void }) {
  const [editing, setEditing] = useState<{ field: SchedulingField; anchor: DOMRect; trigger: HTMLButtonElement } | null>(null);
  return <div className="scheduling-parameters">
    <div><span>优先级</span><span className="priority-value" aria-label={`${account.name}优先级`}>{account.priority}</span></div>
    {(["concurrency", "load_factor"] as const).map(field => <div key={field}><span>{labels[field]}</span><button type="button" aria-label={`编辑${account.name}${labels[field]}`} disabled={!online || !account.operation_versions?.[field] || account.settings_capabilities?.[field]?.editable === false} onClick={e => setEditing({ field, anchor: e.currentTarget.getBoundingClientRect(), trigger: e.currentTarget })}>{field === "load_factor" && account[field] == null ? "自动" : account[field] ?? "—"}</button></div>)}
    {editing && <NumericEditor account={account} online={online} {...editing} close={() => { editing.trigger.focus({ preventScroll: true }); setEditing(null); }}/>}
  </div>;
}
