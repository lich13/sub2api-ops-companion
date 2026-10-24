import DiagnosticText from "./DiagnosticText";
import { useEffect, useId, useRef, useState } from "react";
import { CircleCheck, LoaderCircle, ShieldCheck } from "lucide-react";
import { api } from "./bridge";
import { operationEpoch } from "./accountOperations";

export type ProxyOption = { id: number; name: string; protocol: string; status: string };
type ConnectionResult = { success: boolean; message: string; latency_ms: number | null; ip_address: string; city: string; region: string; country: string; country_code: string };
type QualityResult = { score: number; grade: string; summary: string; exit_ip: string; country: string; base_latency_ms: number | null; items: { target: string; status: "pass" | "warn" | "fail" | "challenge"; http_status: number | null; latency_ms: number | null; message: string }[] };
type Result = { busy?: boolean; error?: string; connection?: ConnectionResult; quality?: QualityResult };
const states = { pass: "通过", warn: "警告", fail: "失败", challenge: "挑战" };
export default function ProxyOptions({ proxies, value, onChange, online, disabled = false, connectionKey = "" }: {
  proxies: ProxyOption[]; value: number | null | undefined; onChange: (value: number | null) => void;
  online: boolean; disabled?: boolean; connectionKey?: string;
}) {
  const [query, setQuery] = useState(""), [results, setResults] = useState<Record<string, Result>>({});
  const generation = useRef(0), running = useRef(new Set<string>()), name = useId();
  useEffect(() => { ++generation.current; setResults({}); running.current.clear(); return () => { ++generation.current; }; }, [connectionKey]);
  async function diagnose(id: number, action: "test" | "quality-check") {
    const key = `${id}:${action}`, token = generation.current, epoch = operationEpoch();
    if (!online || running.current.has(key)) return;
    running.current.add(key); setResults(old => ({ ...old, [key]: { busy: true } }));
    const valid = () => token === generation.current && epoch === operationEpoch();
    try {
      const result = await api<ConnectionResult | QualityResult>("POST", `/proxies/${id}/${action}`);
      if (valid()) setResults(old => ({ ...old, [key]: action === "test" ? { connection: result as ConnectionResult } : { quality: result as QualityResult } }));
    } catch (error) {
      if (valid()) setResults(old => ({ ...old, [key]: { error: String(error).replace(/^Error: /, "") } }));
    } finally { if (valid()) running.current.delete(key); }
  }
  return <div className="proxy-picker">
    <input className="settings-search" type="search" aria-label="搜索代理" placeholder="搜索代理名称或 ID" value={query} onChange={e => setQuery(e.target.value)}/>
    <div className="proxy-options" role="radiogroup" aria-label="账号代理">
      <label><input type="radio" name={name} checked={value === null} disabled={disabled || !online} onChange={() => onChange(null)}/><span>不使用代理</span></label>
      {value != null && !proxies.some(p => p.id === value) && <p className="muted">当前代理 #{value} · 不在可用列表中</p>}
      {proxies.filter(p => `${p.name} ${p.id} ${p.protocol}`.toLowerCase().includes(query.trim().toLowerCase())).map(proxy => {
        const test = results[`${proxy.id}:test`] ?? {}, quality = results[`${proxy.id}:quality-check`] ?? {};
        return <article className="proxy-option" key={proxy.id}>
          <label><input type="radio" name={name} checked={value === proxy.id} disabled={disabled || !online || proxy.status !== "active"} onChange={() => onChange(proxy.id)}/><span><b>{proxy.name}</b><small>#{proxy.id} · {proxy.protocol} · {proxy.status === "active" ? "可用" : "不可用"}</small></span></label>
          <div className="proxy-diagnostic-actions">
            <button type="button" aria-label={`检测代理 ${proxy.id}`} disabled={!online || test.busy} onClick={() => void diagnose(proxy.id, "test")}>{test.busy ? <LoaderCircle className="spin" size={14}/> : <CircleCheck size={14}/>}测试连接</button>
            <button type="button" aria-label={`质量检查代理 ${proxy.id}`} disabled={!online || quality.busy} onClick={() => void diagnose(proxy.id, "quality-check")}>{quality.busy ? <LoaderCircle className="spin" size={14}/> : <ShieldCheck size={14}/>}质量检测</button>
          </div>
          {test.error && <div role="alert" className="bad-text"><DiagnosticText text={test.error}/></div>}
          {test.connection && <div className="proxy-diagnostic-result" role="status"><b>{test.connection.success ? "连接成功" : "连接失败"}</b><span>{test.connection.latency_ms != null && `${test.connection.latency_ms} ms`}</span><span>{[test.connection.ip_address, test.connection.country, test.connection.region, test.connection.city].filter(Boolean).join(" · ")}</span>{test.connection.message && <DiagnosticText text={test.connection.message}/>}</div>}
          {quality.error && <div role="alert" className="bad-text"><DiagnosticText text={quality.error}/></div>}
          {quality.quality && <div className="proxy-diagnostic-result" role="status"><b>{quality.quality.score} 分 · {quality.quality.grade}</b><DiagnosticText text={quality.quality.summary}/><span>{[quality.quality.exit_ip, quality.quality.country].filter(Boolean).join(" · ")}</span>{quality.quality.items.map((item, i) => <div className="proxy-quality-item" key={`${item.target}:${i}`}><span>{item.target}</span><b className={item.status === "fail" ? "bad-text" : ""}>{states[item.status]}</b><span>{item.latency_ms != null ? `${item.latency_ms} ms` : "—"}</span>{item.message && <DiagnosticText text={item.message}/>}</div>)}</div>}
        </article>;
      })}
    </div>
  </div>;
}
