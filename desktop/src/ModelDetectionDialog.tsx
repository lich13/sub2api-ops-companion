import { useEffect, useRef, useState } from "react";
import { LoaderCircle, X } from "lucide-react";
import { api, command } from "./bridge";
import { useBackAction } from "./mobile";
import { fullTime, type Account } from "./types";
type Model = { id: string; display_name?: string };
type Detection = {
  next_allowed_at?: string | null; generation?: number;
  disposition?: { marked: boolean; schedule_verified: boolean };
  disposition_notification?: { status: string } | null;
  recheck_recovery?: { status: string; generation: number; mark_cleared: boolean; reason?: string } | null;
  recovery_notification?: { status: string } | null;
  enabled: boolean; interval_minutes: number; model_id: string; version: string;
  next_at?: string | null; status: string; reason?: string; job_id?: string;
  last_result?: { status?: string; report?: { prediction_name?: string }; completed_at?: string } | null;
};
const status: Record<string, string> = { disabled: "未启用定时检测", waiting: "等待下次检测", paused: "已暂停", queued: "已排队", running: "检测中", handling: "正在处置", completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needs_confirmation: "需确认" };
export default function ModelDetectionDialog({ account, online, close, report, showResult }: { account: Account; online: boolean; close: () => void; report: (error: unknown) => void; showResult?: () => void }) {
  const [value, setValue] = useState<Detection | null>(null), [draft, setDraft] = useState<Detection | null>(null), [models, setModels] = useState<Model[]>([]);
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const generation = useRef(0); useBackAction(true, close);
  async function read() {
    const current = ++generation.current; setLoading(true); setBusy(false); setError("");
    try {
      const [d, m] = await Promise.all([api<Detection>("GET", `/accounts/${account.id}/model-detection`), api<Model[]>("GET", `/accounts/${account.id}/models?purpose=model_test`)]);
      if (current === generation.current) { setValue(d); setDraft(d); setModels(m); }
    } catch (e) { if (current === generation.current) setError(e instanceof Error ? e.message : "检测设置读取失败"); }
    finally { if (current === generation.current) setLoading(false); }
  }
  useEffect(() => { if (online) void read(); return () => { ++generation.current; }; }, [account.id, online]);
  useEffect(() => {
    if (!online || !value) return;
    let alive = true, timer: ReturnType<typeof setTimeout>;
    const poll = async () => { try { const d = await api<Detection>("GET", `/accounts/${account.id}/model-detection`); if (alive) setValue(d); } catch { /* Keep the last verified state. */ } if (alive) timer = setTimeout(() => void poll(), 2000); };
    timer = setTimeout(() => void poll(), 2000); return () => { alive = false; clearTimeout(timer); };
  }, [account.id, online, !!value]);
  async function save() {
    if (!draft || busy || !online) return;
    const current = generation.current; setBusy(true); setError("");
    try {
      const result = await api<Detection>("PUT", `/accounts/${account.id}/model-detection`, { expected_version: draft.version, enabled: draft.enabled, interval_minutes: draft.interval_minutes, model_id: draft.model_id });
      if (current === generation.current) { setValue(result); setDraft(result); await command("refresh"); }
    } catch (e) { if (current === generation.current) { setError(e instanceof Error ? e.message : "检测设置保存失败"); report(e); } }
    finally { if (current === generation.current) setBusy(false); }
  }
  async function cancel() { if (!value?.job_id) return; const current = generation.current; setBusy(true); try { await api("POST", `/model-tests/${value.job_id}/cancel`, {}); if (current === generation.current) await read(); } catch (e) { if (current === generation.current) report(e); } finally { if (current === generation.current) setBusy(false); } }
  const available = !!draft && models.some((m) => m.id === draft.model_id);
  const recovery = value?.recheck_recovery?.generation === value?.generation ? value?.recheck_recovery : null;
  const notification = recovery ? value?.recovery_notification : value?.disposition_notification;
  return <div className="modal-backdrop" onClick={close}><section className="model-detection-dialog" role="dialog" aria-modal="true" aria-label="定时检测" onClick={(e) => e.stopPropagation()}><header><div><span className="eyebrow">{account.name} #{account.id}</span><h2>定时检测</h2></div><button className="icon-button" aria-label="关闭定时检测" onClick={close}><X size={19}/></button></header>{loading ? <LoaderCircle className="spin"/> : draft && value && <><label className="switch-row"><span>启用定时检测</span><input type="checkbox" checked={draft.enabled} disabled={busy || !online} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}/></label><label className="field"><span>检测间隔（分钟）</span><input aria-label="检测间隔" type="number" min={1} step={1} value={draft.interval_minutes} disabled={busy || !online} onChange={(e) => setDraft({ ...draft, interval_minutes: Number(e.target.value) })}/></label><label className="field"><span>检测模型</span><select aria-label="检测模型" value={draft.model_id} disabled={busy || !online || !models.length} onChange={(e) => setDraft({ ...draft, model_id: e.target.value })}>{!available && <option value={draft.model_id}>{draft.model_id}（当前不可用）</option>}{models.map((m) => <option key={m.id} value={m.id}>{m.display_name || m.id}</option>)}</select></label>{!available && <p className="bad-text">所选模型不在当前分组白名单中。</p>}<dl className="detection-meta"><dt>状态</dt><dd>{status[value.status] || "等待核对"}</dd><dt>下次执行</dt><dd>{fullTime(value.next_at)}</dd>{value.reason && <><dt>原因</dt><dd>{value.reason}</dd></>}{value.next_allowed_at && Date.parse(value.next_allowed_at) > Date.now() && <><dt>冷却至</dt><dd>{fullTime(value.next_allowed_at)}</dd></>}{recovery ? <><dt>复查结果</dt><dd>{recovery.status === "completed" && recovery.mark_cleared ? "已取消降智标记 · 调度保持不变" : recovery.reason || "正在核对标记"}</dd></> : value.disposition && <><dt>处置</dt><dd>{value.disposition.marked ? "标记已保存" : "标记待保存"} · {value.disposition.schedule_verified ? "停调度已确认" : "停调度待确认"}</dd></>}{notification && <><dt>结果通知</dt><dd>{({queued: "待推送", retry: "待重试", delivered: "已推送", suppressed: "已取消"} as Record<string, string>)[notification.status] ?? "—"}</dd></>}<dt>最近结果</dt><dd>{value.last_result?.report?.prediction_name || status[value.last_result?.status || ""] || "—"}</dd></dl>{value.job_id && <div className="account-template-actions"><button onClick={showResult}>查看任务</button>{["queued", "running", "retrying"].includes(value.status) && <button disabled={busy || !online} onClick={() => void cancel()}>取消任务</button>}</div>}<footer><button disabled={busy || !online} onClick={() => void read()}>重新读取</button><button className="primary" disabled={busy || !online || !Number.isInteger(draft.interval_minutes) || draft.interval_minutes < 1 || (!available && draft.enabled)} onClick={() => void save()}>{busy && <LoaderCircle size={15} className="spin"/>}保存</button></footer></>}{error && <p className="bad-text" role="alert">{error} <button onClick={() => void read()}>重试</button></p>}</section></div>;
}
