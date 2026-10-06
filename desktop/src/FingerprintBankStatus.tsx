import { useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { api } from "./bridge";
import { fullTime } from "./types";
export type BankVersion = { revision: string; sha256: string; built_at: string; analyzer_version: number };
type Bank = { version: BankVersion; source: string; status: string; result?: string; checked_at?: number; synced_at?: number; cooldown_until?: number; error?: string };
const errors: Record<string, string> = { network: "网络失败", timeout: "网络超时", "rate-limit": "限流等待", cache: "缓存保存失败", incompatible: "需要升级应用", "invalid-data": "指纹库校验失败" };
const time = (value?: number) => value ? fullTime(new Date(value * 1000).toISOString()) : "—";
export default function FingerprintBankStatus({ online, taskVersion }: { online: boolean; taskVersion?: BankVersion | null }) {
  const [value, setValue] = useState<Bank | null>(null), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const generation = useRef(0), timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  async function read(current: number, follow = false) {
    try {
      const next = await api<Bank>("GET", "/modeltrace/fingerprint-bank");
      if (generation.current !== current) return;
      if (!next?.version) throw new Error("指纹库状态暂不可用");
      setValue(next); setError("");
      if (next.status === "checking" || follow) timer.current = setTimeout(() => void read(current), 1000);
      else setBusy(false);
    } catch (e) { if (generation.current === current) { setError(e instanceof Error ? e.message : "指纹库读取失败"); setBusy(false); } }
  }
  useEffect(() => { const current = ++generation.current; if (online) void read(current); return () => { ++generation.current; clearTimeout(timer.current); }; }, [online]);
  async function sync() {
    const current = ++generation.current; clearTimeout(timer.current); setBusy(true); setError("");
    try { await api("POST", "/modeltrace/fingerprint-bank/sync", {}); if (current === generation.current) await read(current, true); }
    catch (e) { if (current === generation.current) { setBusy(false); setError(e instanceof Error ? e.message : "检查失败"); } }
  }
  const result = error || (busy || value?.status === "checking" ? "检查中" : value?.error ? errors[value.error] || "检查失败" : value?.result === "updated" ? "已更新" : value?.result === "up-to-date" ? "已是最新" : "");
  return <div className="fingerprint-status"><div className="fingerprint-status-row"><details><summary>指纹库 {value?.version.revision.slice(0, 8) || "—"}</summary>{value && <dl><dt>来源</dt><dd>{({ bundled: "内置库", cache: "有效缓存", remote: "官方远程库" } as Record<string, string>)[value.source] || value.source}</dd><dt>构建时间</dt><dd>{fullTime(value.version.built_at)}</dd><dt>最近检查</dt><dd>{time(value.checked_at)}</dd><dt>最近更新</dt><dd>{time(value.synced_at)}</dd><dt>下次重试</dt><dd>{time(value.cooldown_until)}</dd><dt>SHA-256</dt><dd>{value.version.sha256}</dd>{taskVersion && <><dt>本轮使用</dt><dd>{taskVersion.revision.slice(0, 8)} · 分析器 {taskVersion.analyzer_version}<br/>{taskVersion.sha256}</dd></>}</dl>}</details><button className="text-button" disabled={!online || busy} onClick={() => void sync()}><RefreshCw size={14} className={busy ? "spin" : ""}/>检查更新</button></div>{result && <span className={error || value?.error ? "bad-text" : "muted"} role="status">{result}</span>}</div>;
}
