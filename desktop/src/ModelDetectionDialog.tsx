import { useEffect, useRef, useState } from "react";
import { LoaderCircle, X } from "lucide-react";
import { api, command } from "./bridge";
import { useBackAction } from "./mobile";
import { fullTime, type Account } from "./types";
type Model = { id: string; display_name?: string };
type Detection = {
  last_automatic_started_at?: string | null;
  next_allowed_at?: string | null; generation?: number;
  disposition?: { marked: boolean; schedule_verified: boolean };
  disposition_notification?: { status: string } | null;
  recheck_recovery?: { status: string; generation: number; mark_cleared: boolean; reason?: string } | null;
  recovery_notification?: { status: string } | null;
  enabled: boolean; interval_minutes: number; model_id: string; model_source?: "global" | "account"; model_override?: string | null; global_model_id?: string; version: string;
  next_at?: string | null; status: string; reason?: string; job_id?: string;
  last_result?: { status?: string; report?: { prediction_name?: string }; completed_at?: string } | null;
};
const status: Record<string, string> = { disabled: "未启用定时检测", waiting: "等待下次检测", paused: "已暂停", queued: "已排队", running: "检测中", handling: "正在处置", completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needs_confirmation: "需确认" };
export default function ModelDetectionDialog({ account, online, close, report, showResult }: { account: Account; online: boolean; close: () => void; report: (error: unknown) => void; showResult?: () => void }) {
  const [value, setValue] = useState<Detection | null>(null), [draft, setDraft] = useState<Detection | null>(null), [models, setModels] = useState<Model[]>([]);
  const [modelsReady, setModelsReady] = useState(false);
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const closed = useRef(false);
  const generation = useRef(0), loadedAccount = useRef(account.id), draftRef = useRef(draft); draftRef.current = draft;
  const dismiss = () => { closed.current = true; ++generation.current; close(); };
  useBackAction(true, dismiss);
  async function read(replaceDraft = false) {
    const current = ++generation.current; setModelsReady(false); if (!draftRef.current) setLoading(true); setBusy(false); setError("");
    try {
      const [settings, directory] = await Promise.allSettled([api<Detection>("GET", `/accounts/${account.id}/model-detection`), api<Model[]>("GET", `/accounts/${account.id}/models?purpose=model_test`)]);
      if (current !== generation.current || closed.current) return;
      if (settings.status === "rejected") throw settings.reason;
      const d = settings.value;
      setValue(d); setDraft((previous) => replaceDraft
        ? directory.status === "fulfilled" || !previous ? d : { ...d, enabled: previous.enabled, interval_minutes: previous.interval_minutes, model_override: previous.model_override }
        : previous ?? d);
      if (directory.status === "fulfilled") { setModelsReady(true); setModels(directory.value); }
      else setError(directory.reason instanceof Error ? directory.reason.message : "模型候选读取失败");
    } catch (e) { if (current === generation.current) setError(e instanceof Error ? e.message : "检测设置读取失败"); }
    finally { if (current === generation.current) setLoading(false); }
  }
  useEffect(() => { closed.current = false; if (loadedAccount.current !== account.id) { loadedAccount.current = account.id; draftRef.current = null; setModelsReady(false); setDraft(null); setValue(null); setModels([]); } if (online) void read(); return () => { ++generation.current; }; }, [account.id, online]);
  useEffect(() => {
    if (!online || !value) return;
    let alive = true, timer: ReturnType<typeof setTimeout>;
    const poll = async () => { const current = generation.current; try { const d = await api<Detection>("GET", `/accounts/${account.id}/model-detection`); if (alive && !closed.current && current === generation.current) setValue(d); } catch { /* Keep the last verified state. */ } if (alive && !closed.current) timer = setTimeout(() => void poll(), 2000); };
    timer = setTimeout(() => void poll(), 2000); return () => { alive = false; clearTimeout(timer); };
  }, [account.id, online, !!value]);
  async function save() {
    if (!draft || busy || !online || !supported) return;
    const current = ++generation.current; setBusy(true); setError("");
    try {
      const result = await api<Detection>("PUT", `/accounts/${account.id}/model-detection`, { expected_version: draft.version, enabled: draft.enabled, interval_minutes: draft.interval_minutes, ...(canChooseModel ? { model_override: draft.model_override ?? null } : {}) });
      if (current === generation.current) { setValue(result); setDraft(result); await command("refresh"); }
    } catch (e) { if (current === generation.current) { setError(e instanceof Error ? e.message : "检测设置保存失败"); report(e); } }
    finally { if (current === generation.current) setBusy(false); }
  }
  async function cancel() { if (!value?.job_id) return; const current = ++generation.current; setBusy(true); try { await api("POST", `/model-tests/${value.job_id}/cancel`, {}); if (current === generation.current) await read(); } catch (e) { if (current === generation.current) report(e); } finally { if (current === generation.current) setBusy(false); } }
  const keyAccount = account.platform === "openai" && account.type === "apikey";
  const canChooseModel = keyAccount && value?.model_override !== undefined;
  const selectedModel = draft?.model_override ?? value?.global_model_id ?? value?.model_id ?? "";
  const available = models.some((m) => m.id === selectedModel);
  const supported = value?.model_source === "global" || value?.model_source === "account";
  const changedModel = !!value && !!draft && value.model_id !== draft.model_id;
  const recovery = value?.recheck_recovery?.generation === value?.generation ? value?.recheck_recovery : null;
  const lastStarted = value?.last_automatic_started_at;
  const lastDetection = lastStarted ? fullTime(lastStarted) : value?.last_result || lastStarted === "" ? "时间未知" : "暂无记录";
  const notification = recovery ? value?.recovery_notification : value?.disposition_notification;
  return <div className="modal-backdrop" onClick={dismiss}><section className="model-detection-dialog" role="dialog" aria-modal="true" aria-label="定时检测" onClick={(e) => e.stopPropagation()}><header><div><span className="eyebrow">{account.name} #{account.id}</span><h2>定时检测</h2></div><button className="icon-button" aria-label="关闭定时检测" onClick={dismiss}><X size={19}/></button></header>{loading ? <LoaderCircle className="spin"/> : draft && value && <><label className="switch-row"><span>启用定时检测</span><input type="checkbox" checked={draft.enabled} disabled={busy || !online || !supported} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}/></label><label className="field"><span>检测间隔（分钟）</span><input aria-label="检测间隔" type="number" min={1} step={1} value={draft.interval_minutes} disabled={busy || !online || !supported} onChange={(e) => setDraft({ ...draft, interval_minutes: Number(e.target.value) })}/></label>{keyAccount && <label className="field"><span>检测模型</span><select aria-label="检测模型" value={draft.model_override ?? ""} disabled={busy || !online || !canChooseModel || !modelsReady || !!error} onChange={(e) => setDraft({ ...draft, model_override: e.target.value || null })}><option value="">跟随全局（{value.global_model_id ?? value.model_id}）</option>{draft.model_override && !models.some((m) => m.id === draft.model_override) && <option value={draft.model_override}>{draft.model_override}（当前不可用）</option>}{models.map((model) => <option key={model.id} value={model.id}>{model.display_name || model.id}</option>)}</select>{!canChooseModel && <span className="muted">请升级服务端以单独指定检测模型。</span>}</label>}{!supported && <p className="bad-text" role="alert">请升级服务端以使用全局自动检测模型。</p>}{changedModel && <p className="bad-text">自动检测模型已变化，请重新读取后保存；当前草稿已保留。</p>}{modelsReady && online && !error && !available && <p className="bad-text">所选模型不在当前账号上游模型目录中。</p>}<dl className="detection-meta"><dt>状态</dt><dd>{status[value.status] || "等待核对"}</dd><dt>上次检测（开始）</dt><dd>{lastDetection}</dd><dt>下次执行</dt><dd>{fullTime(value.next_at)}</dd>{value.reason && <><dt>原因</dt><dd>{value.reason}</dd></>}{value.next_allowed_at && Date.parse(value.next_allowed_at) > Date.now() && <><dt>冷却至</dt><dd>{fullTime(value.next_allowed_at)}</dd></>}{recovery ? <><dt>复查结果</dt><dd>{recovery.status === "completed" && recovery.mark_cleared ? "已取消降智标记 · 调度保持不变" : recovery.reason || "正在核对标记"}</dd></> : value.disposition && <><dt>处置</dt><dd>{value.disposition.marked ? "标记已保存" : "标记待保存"} · {value.disposition.schedule_verified ? "停调度已确认" : "停调度待确认"}</dd></>}{notification && <><dt>结果通知</dt><dd>{({queued: "待推送", retry: "待重试", delivered: "已推送", suppressed: "已取消"} as Record<string, string>)[notification.status] ?? "—"}</dd></>}<dt>最近结果</dt><dd>{value.last_result?.report?.prediction_name || status[value.last_result?.status || ""] || "—"}</dd></dl>{value.job_id && <div className="account-template-actions"><button onClick={showResult}>查看任务</button>{["queued", "running", "retrying"].includes(value.status) && <button disabled={busy || !online || !supported} onClick={() => void cancel()}>取消任务</button>}</div>}<footer><button disabled={busy || !online} onClick={() => void read(true)}>重新读取</button><button className="primary" disabled={busy || !online || !supported || changedModel || !Number.isInteger(draft.interval_minutes) || draft.interval_minutes < 1 || ((!modelsReady || !available || !!error) && draft.enabled)} onClick={() => void save()}>{busy && <LoaderCircle size={15} className="spin"/>}保存</button></footer></>}{error && <p className="bad-text" role="alert">{error} <button onClick={() => void read()}>重试</button></p>}</section></div>;
}
