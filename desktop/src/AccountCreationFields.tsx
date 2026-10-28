import { useEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Plus, X } from "lucide-react";
import { fields, fieldVisible, getField, labels, mappingPresets, modelPresets, protocols, tierOptions, urlPresets, type CreationDraft, type Field, type RecordValue } from "./accountCreation";

type Props = {
  draft: CreationDraft; change: (path: string, value: unknown) => void;
  invalid: (path: string, error: string) => void;
  tls: { id: number; name: string }[]; aiStudio: boolean; quotaNotify: boolean; webSearch: boolean; groupLongContext: boolean;
};
function Pairs({ field, value, change, invalid, draft }: { field: Field; value: unknown; change: (value: unknown) => void; invalid: (error: string) => void; draft: CreationDraft }) {
  const entries = (value: unknown): [string,string][] => Object.entries(value && typeof value === "object" ? value : {}).map(([k,v]) => [k,String(v)]);
  const [rows,setRows] = useState(() => entries(value)), last = useRef(value);
  useEffect(() => { if (last.current !== value) { last.current = value; setRows(entries(value)); invalid(""); } }, [value]);
  function update(next: [string,string][]) {
    setRows(next); const out = Object.fromEntries(next); last.current = out; change(out);
    invalid(next.some(([key,val]) => !key.trim() || field.kind === "mapping" && !val.trim()) ? `${field.label}存在未填项目` : new Set(next.map(([key]) => field.kind === "headers" ? key.toLowerCase() : key)).size !== next.length ? `${field.label}存在重复名称` : "");
  }
  return <div className="creation-collection">
    <div className="creation-inline"><strong>{field.label}</strong><button type="button" onClick={() => update([...rows,["",""]])}><Plus size={14}/>添加{field.kind === "headers" ? "请求头" : "映射"}</button>
      {field.path === "credentials.model_mapping" && <><button type="button" onClick={() => update(modelPresets(draft.platform).map(m => [m,m]))}>填入默认白名单</button><button type="button" onClick={() => update([])}>不限制模型</button></>}
    </div>
    {field.kind === "mapping" && <p className="muted">相同的请求与转发模型表示白名单；留空表示不限制。</p>}
    {field.path === "credentials.model_mapping" && <select aria-label="添加预设映射" value="" onChange={e => { const p = mappingPresets(draft)[Number(e.target.value)]; if(p)update([...rows.filter(([key]) => key !== p.from),[p.from,p.to]]); }}><option value="">添加预设映射…</option>{mappingPresets(draft).map((p,i) => <option key={i} value={i}>{p.label}</option>)}</select>}
    {rows.map(([key,value],i) => <div className="creation-pair" key={i}>
      <input aria-label={`${field.label}名称 ${i+1}`} placeholder={field.kind === "headers" ? "请求头名称" : "请求模型"} value={key} onChange={e => update(rows.map((r,n) => n===i ? [e.target.value,r[1]] : r))}/>
      <input aria-label={`${field.label}值 ${i+1}`} placeholder={field.kind === "headers" ? "请求头值" : "转发模型"} value={value} onChange={e => update(rows.map((r,n) => n===i ? [r[0],e.target.value] : r))}/>
      <button type="button" className="icon-button" aria-label={`移除${field.label} ${i+1}`} onClick={() => update(rows.filter((_,n) => n!==i))}><X size={14}/></button>
    </div>)}
  </div>;
}
function Rules({ field,value,change }: { field: Field; value: unknown; change: (value: unknown) => void }) {
  const rows = Array.isArray(value) ? value as RecordValue[] : [], pause = field.kind === "pause-rules";
  const update = (index: number,key: string,value: unknown) => change(rows.map((r,i) => i===index ? {...r,[key]:value} : r));
  function move(i: number, delta: number) { const copy=[...rows]; [copy[i],copy[i+delta]]=[copy[i+delta],copy[i]]; change(copy); }
  return <div className="creation-collection"><div className="creation-inline"><strong>{field.label}</strong><button type="button" onClick={() => change([...rows,pause ? {error_code:429,keywords:[],duration_minutes:30,description:""} : {pattern:"",protocol:"chat_completions"}])}><Plus size={14}/>添加规则</button></div>
    {rows.map((r,i) => <div className="creation-rule" key={i}>
      <div className="creation-inline"><strong>规则 {i+1}</strong><button type="button" aria-label={`上移规则 ${i+1}`} disabled={i===0} onClick={() => move(i,-1)}><ArrowUp size={14}/></button><button type="button" aria-label={`下移规则 ${i+1}`} disabled={i===rows.length-1} onClick={() => move(i,1)}><ArrowDown size={14}/></button><button type="button" aria-label={`移除规则 ${i+1}`} onClick={() => change(rows.filter((_,n) => n!==i))}><X size={14}/></button></div>
      {pause ? <div className="creation-grid"><label>状态码<input aria-label={`状态码 ${i+1}`} type="number" min={100} max={599} value={r.error_code === "" || r.error_code == null || Number.isNaN(r.error_code) ? "" : String(r.error_code)} onChange={e => update(i,"error_code",e.target.value === "" ? "" : Number(e.target.value))}/></label><label>停用分钟<input aria-label={`停用分钟 ${i+1}`} type="number" min={1} value={r.duration_minutes === "" || r.duration_minutes == null || Number.isNaN(r.duration_minutes) ? "" : String(r.duration_minutes)} onChange={e => update(i,"duration_minutes",e.target.value === "" ? "" : Number(e.target.value))}/></label><label className="creation-wide">关键词（逗号分隔）<input aria-label={`关键词 ${i+1}`} value={Array.isArray(r.keywords) ? r.keywords.join(",") : ""} onChange={e => update(i,"keywords",e.target.value.split(/[,，]/))}/></label><label className="creation-wide">说明<input aria-label={`规则说明 ${i+1}`} value={String(r.description??"")} onChange={e => update(i,"description",e.target.value)}/></label></div> : <div className="creation-grid"><label>模型匹配<input aria-label={`模型匹配 ${i+1}`} value={String(r.pattern??"")} onChange={e => update(i,"pattern",e.target.value)}/></label><label>协议<select aria-label={`规则协议 ${i+1}`} value={String(r.protocol)} onChange={e => update(i,"protocol",e.target.value)}>{protocols("opencode_go").map(p => <option key={p} value={p}>{labels[p]}</option>)}</select></label></div>}
    </div>)}
  </div>;
}
export default function AccountCreationFields({ draft,change,invalid,tls,aiStudio,quotaNotify,webSearch,groupLongContext }: Props) {
  const shown = fields.filter(f => fieldVisible(f,draft) && (f.group !== "额度通知" || quotaNotify) && (f.path !== "extra.web_search_emulation" || webSearch) && (f.path !== "extra.openai_long_context_billing_enabled" || !groupLongContext));
  function control(f: Field) {
    const value=getField(draft,f.path), set=(v: unknown) => change(f.path,v);
    if (f.kind === "mapping" || f.kind === "headers") return <Pairs key={f.path} field={f} draft={draft} value={value} change={set} invalid={error => invalid(f.path,error)}/>;
    if (f.kind === "pause-rules" || f.kind === "protocol-rules") return <Rules field={f} value={value} change={set}/>;
    if (f.kind === "boolean" || f.kind === "stringbool") return <label className="creation-toggle"><input type="checkbox" checked={value === true || value === "true"} onChange={e => set(f.kind === "stringbool" ? String(e.target.checked) : e.target.checked)}/>{f.label}</label>;
    if (f.kind === "strings") return <fieldset className="creation-choices"><legend>{f.label}</legend>{f.choices?.map(p => <label key={p}><input type="checkbox" checked={Array.isArray(value) && value.includes(p)} onChange={e => { const next = new Set(Array.isArray(value) ? value as string[] : []); if(e.target.checked) next.add(p); else if(next.size>1)next.delete(p);set([...next]); }}/>{p === "seedance" ? "Seedance (Ark)" : labels[p]||p}</label>)}</fieldset>;
    if (f.kind === "protocols") return <div className="creation-collection"><strong>{f.label}</strong>{protocols(draft.platform).map(p => <label key={p}>{labels[p]}<input aria-label={`${labels[p]} 地址`} type="url" value={String((value as RecordValue)?.[p]??"")} onChange={e => set({...value as RecordValue,[p]:e.target.value})}/></label>)}{urlPresets(draft).map(p => <button type="button" key={`${p.url}:${p.protocol}`} onClick={() => set({...value as RecordValue,[p.protocol]:p.url})}>{p.label}</button>)}</div>;
    if (f.kind === "tls") return <label>{f.label}<select value={String(value??"")} onChange={e => set(e.target.value ? Number(e.target.value) : null)}><option value="">默认配置</option>{tls.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>;
    if (f.kind === "select" || f.kind === "tier") {
      const choices=f.kind === "tier" ? tierOptions(draft) : (f.choices??[]).filter(c => !(f.path === "credentials.api_protocol" && draft.platform === "zhipu" && c === "responses"));
      return <label>{f.label}<select value={String(value??f.default??"")} onChange={e => set(e.target.value === "" && !choices.includes("") ? null : e.target.value)}>{!choices.includes("") && f.default === undefined && <option value="">未设置</option>}{choices.map(c => <option key={c} value={c} disabled={f.path === "credentials.oauth_type" && c === "ai_studio" && !aiStudio}>{labels[c]||c||"关闭"}{c === "ai_studio" && !aiStudio ? "（未配置）" : ""}</option>)}</select></label>;
    }
    return <label>{f.label}<input type={f.kind === "number" ? "number" : f.kind === "url" ? "url" : "text"} min={f.min} max={f.max} step={f.kind === "number" ? "any" : undefined} value={Array.isArray(value) ? value.join(", ") : String(value??"")} placeholder={f.kind === "numbers" ? "逗号分隔" : f.kind === "number" ? "未设置" : undefined} onChange={e => set(f.kind === "number" ? e.target.value === "" ? null : Number(e.target.value) : e.target.value)}/>{f.path === "credentials.base_url" && urlPresets(draft).map(p => <button type="button" key={p.url} onClick={() => set(p.url)}>{p.label}</button>)}</label>;
  }
  return <>{[...new Set(shown.map(f => f.group))].map(group => <details className="creation-section" key={group} open={group === "连接" || group === "调度"}><summary>{group}</summary><div className="creation-grid">{shown.filter(f => f.group===group).map(f => <div className={["mapping","headers","pause-rules","protocols","protocol-rules","strings"].includes(f.kind) ? "creation-wide" : ""} key={f.path}>{control(f)}</div>)}</div></details>)}</>;
}
