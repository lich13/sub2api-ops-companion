import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Play, RotateCcw, Square, X } from "lucide-react";
import { api } from "./bridge";
import type { Account } from "./types";
import FingerprintBankStatus, { type BankVersion } from "./FingerprintBankStatus";
import { useBackAction } from "./mobile";

type Model = { id: string; display_name?: string; type?: string };
type Result = {
  id: string; account_id: number; account_name: string; requested_model: string;
  forwarded_model: string; returned_models: string[]; status: string;
  completed_groups: number; valid_groups: number; attempts: number;
  concurrency?: number; groups?: { index: number; status: string; attempts: number; ttft_ms: number | null; duration_ms: number | null; error?: string; diagnostics?: { protocol?: string; response_protocol?: string; http_status?: number; content_type?: string; first_event_ms?: number; first_event_type?: string; last_event_type?: string; headers_ms?: number; bytes?: number; end_reason?: string; output_tokens?: number; reasoning_tokens?: number; max_output_tokens?: number } }[];
  bank_version?: BankVersion; automatic_disposition?: { marked?: boolean; schedule_verified?: boolean; reason?: string };
  can_retry?: boolean; completion_reason?: string;
  duration_ms: number; error?: string; report?: { prediction_name?: string; prediction?: string; probability?: number; used_outputs?: number } | null;
};
const terminal = new Set(["completed", "failed", "cancelled", "interrupted", "needs_confirmation"]);

export default function ModelTestDialog({ account, online, close, report, concurrency = 1, saveConcurrency }: {
  account: Account; online: boolean; close: () => void; report: (error: unknown) => void;
  concurrency?: number; saveConcurrency?: (value: number) => void;
}) {
  const [slots, setSlots] = useState([1, 2, 3].includes(concurrency) ? concurrency : 1);
  const [models, setModels] = useState<Model[]>([]), [model, setModel] = useState("");
  const modelsRef = useRef<Model[]>([]);
  const selectionTouched = useRef(false), submitted = useRef(false);
  const [job, setJob] = useState<Result | null>(null), [loading, setLoading] = useState(true);
  const [modelError, setModelError] = useState(""), [modelReload, setModelReload] = useState(0);
  const [busy, setBusy] = useState(false), generation = useRef(0), timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const reportRef = useRef(report); reportRef.current = report;
  useBackAction(true, close);
  useEffect(() => {
    let alive = true;
    setLoading(true); setModelError("");
    void api<Model[]>("GET", `/accounts/${account.id}/models?purpose=model_test`).then((items) => {
      if (!alive) return;
      modelsRef.current = items; setModels(items); setModel((old) => old && items.some((item) => item.id === old) ? old : items[0]?.id ?? "");
    }).catch((e) => { if (alive) { setModels([]); setModel(""); setModelError(e instanceof Error ? e.message : "模型候选读取失败"); reportRef.current(e); } }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; ++generation.current; clearTimeout(timer.current); };
  }, [account.id, modelReload]);
  useEffect(() => {
    let alive = true;
    void api<Result | null>("GET", `/accounts/${account.id}/model-tests/latest`).then((value) => {
      if (alive && value && !submitted.current) {
        setJob(value);
        if (!selectionTouched.current) setModel((old) => modelsRef.current.length && !modelsRef.current.some((item) => item.id === value.requested_model) ? old : value.requested_model);
      }
    }).catch(() => {});
    return () => { alive = false; };
  }, [account.id]);
  useEffect(() => {
    if (!job || terminal.has(job.status) || !online) return;
    const current = ++generation.current;
    const poll = async () => {
      try {
        const next = await api<Result>("GET", `/model-tests/${job.id}`);
        if (generation.current === current) setJob(next);
      } catch (e) { if (generation.current === current) reportRef.current(e); }
      if (generation.current === current) timer.current = setTimeout(() => void poll(), 2000);
    };
    timer.current = setTimeout(() => void poll(), 400);
    return () => { ++generation.current; clearTimeout(timer.current); };
  }, [job?.id, job?.status, online]);
  async function start() {
    if (busy || !online || !model) return;
    submitted.current = true;
    setBusy(true);
    try {
      const next = await api<Result>("POST", `/accounts/${account.id}/model-tests`, {
        model_id: model, expected_version: account.version,
        ...(account.operation_versions?.model_test ? { expected_operation_version: account.operation_versions.model_test } : {}),
        concurrency: slots,
        request_id: `${crypto.randomUUID().replaceAll("-", "").slice(0, 32)}`,
      });
      setJob(next);
    } catch (e) { reportRef.current(e); } finally { setBusy(false); }
  }
  async function cancel() {
    if (!job || busy || (terminal.has(job.status) && job.status !== "needs_confirmation")) return;
    setBusy(true); try { setJob(await api<Result>("POST", `/model-tests/${job.id}/cancel`, {})); } catch (e) { reportRef.current(e); } finally { setBusy(false); }
  }
  async function retryFailed() {
    if (!job || busy) return;
    setBusy(true);
    try { setJob(await api<Result>("POST", `/model-tests/${job.id}/retry`, { request_id: crypto.randomUUID() })); }
    catch (e) { reportRef.current(e); } finally { setBusy(false); }
  }
  const running = !!job && !terminal.has(job.status);
  const reportResult = job?.report;
  return <div className="modal-backdrop" onClick={close}><section className="model-test-dialog" role="dialog" aria-modal="true" aria-label="模型测试" onClick={(e) => e.stopPropagation()}>
    <header><div><h2>模型测试</h2><strong>{account.name} <span>#{account.id}</span></strong></div><button className="icon-button" aria-label="关闭模型测试" onClick={close}><X size={18}/></button></header>
    <label className="model-test-model">模型<select aria-label="测试模型" value={model} disabled={loading || !!modelError || !models.length || running || busy} onChange={(e) => { selectionTouched.current = true; setModel(e.target.value); }}>{models.map((item) => <option key={item.id} value={item.id}>{item.display_name || item.id}</option>)}</select>
      {modelError ? <span className="bad-text" role="alert">{modelError} <button type="button" className="link-button" onClick={() => { setModelError(""); setLoading(true); setModelReload((value) => value + 1); }}>重试</button></span> : !loading && !models.length ? <span className="muted">暂无可用模型</span> : null}
    </label>
    <label className="model-test-model">并发<select aria-label="测试并发" value={slots} disabled={running || busy} onChange={(e) => { const value = Number(e.target.value); setSlots(value); saveConcurrency?.(value); }}>{[1, 2, 3].map((value) => <option key={value} value={value}>{value}</option>)}</select></label>
    {job && <div className="model-test-result" aria-live="polite">
      <div className="model-test-status">{job.status === "running" || job.status === "retrying" ? <LoaderCircle size={16} className="spin"/> : null}<span>{job.status === "queued" ? "已排队" : job.status === "needs_confirmation" ? "账号已变化，需重新确认" : job.status === "completed" ? job.completion_reason === "automatic_degradation" ? "首组命中降智模型" : job.completion_reason === "confidence_99" ? "已提前完成" : "已完成" : job.status === "cancelled" ? "已停止" : job.status === "interrupted" ? "服务重启后中断" : job.status === "failed" ? "测试失败" : `${job.completed_groups}/3 组`}</span><span>{job.attempts} 次请求</span></div>
      {job.groups && <div className="model-test-groups">{job.groups.map((group) => <div key={group.index}><strong>第 {group.index} 组</strong><span>{({ queued: "等待", running: "等待响应", receiving: "接收中", analyzing: "分析中", skipped: "提前结束", retrying: "等待重试", completed: "完成", failed: "失败", cancelled: "已停止" } as Record<string, string>)[group.status] ?? group.status}</span><span>{group.attempts} 次</span><span>首字 {group.ttft_ms == null ? "—" : `${(group.ttft_ms / 1000).toFixed(1)}s`}</span><span>耗时 {group.duration_ms == null ? "—" : `${(group.duration_ms / 1000).toFixed(1)}s`}</span>{group.error && <span className="bad-text">{group.error}</span>}</div>)}</div>}
      <dl><div><dt>请求模型</dt><dd>{job.requested_model}</dd></div><div><dt>转发模型</dt><dd>{job.forwarded_model}</dd></div><div><dt>返回模型</dt><dd>{job.returned_models.length ? job.returned_models.join("、") : "—"}</dd></div><div><dt>指纹推测</dt><dd>{reportResult?.prediction_name || "—"}</dd></div><div><dt>匹配度</dt><dd>{reportResult?.probability == null ? "—" : `${(reportResult.probability * 100).toFixed(1)}%`}</dd></div><div><dt>有效组数</dt><dd>{reportResult?.used_outputs ?? job.valid_groups}/3</dd></div><div><dt>耗时</dt><dd>{job.duration_ms ? `${(job.duration_ms / 1000).toFixed(1)}s` : "—"}</dd></div></dl>
      {job.groups?.some((group) => group.diagnostics) && <details className="model-test-diagnostics"><summary>请求诊断</summary>{job.groups.filter((group) => group.diagnostics).map((group) => <dl key={group.index}><div><dt>第 {group.index} 组</dt><dd>{group.diagnostics?.response_protocol || group.diagnostics?.protocol || '—'}</dd></div><div><dt>HTTP</dt><dd>{group.diagnostics?.http_status ?? '—'} · {group.diagnostics?.content_type || '—'}</dd></div><div><dt>首事件</dt><dd>{group.diagnostics?.first_event_type || '—'} · {group.diagnostics?.first_event_ms == null ? '—' : `${(group.diagnostics.first_event_ms / 1000).toFixed(2)}s`}</dd></div><div><dt>末事件</dt><dd>{group.diagnostics?.last_event_type || '—'}</dd></div><div><dt>输出 Token</dt><dd>{group.diagnostics?.output_tokens ?? '—'}{group.diagnostics?.max_output_tokens != null ? ` / ${group.diagnostics.max_output_tokens}` : ''}</dd></div><div><dt>思考 Token</dt><dd>{group.diagnostics?.reasoning_tokens ?? '—'}</dd></div><div><dt>接收</dt><dd>{group.diagnostics?.bytes ?? 0} bytes</dd></div><div><dt>结束</dt><dd>{group.diagnostics?.end_reason || '—'}</dd></div></dl>)}</details>}
      {job.automatic_disposition && <p>{job.automatic_disposition.marked ? "标记已保存" : "标记待核对"} · {job.automatic_disposition.schedule_verified ? "停调度已确认" : "停调度未确认"}{job.automatic_disposition.reason && `：${job.automatic_disposition.reason}`}</p>}
      {job.error && <p className="bad-text">{job.error}</p>}
    </div>}
    <FingerprintBankStatus online={online} taskVersion={job?.bank_version}/>
    <footer>{job?.can_retry && !running && <button disabled={busy || !online} onClick={() => void retryFailed()}>重试失败组</button>}{job?.status === "needs_confirmation" && <button disabled={busy || !online} onClick={() => void cancel()}>取消任务</button>}{running ? <button className="danger-text" disabled={busy} onClick={() => void cancel()}><Square size={15}/>停止</button> : <><button disabled={loading || !!modelError || !models.length || busy || !model || !online} onClick={() => void start()}>{busy ? <LoaderCircle size={15} className="spin"/> : job ? <RotateCcw size={15}/> : <Play size={15}/>} {job ? "重新测试" : "开始测试"}</button></>}</footer>
  </section></div>;
}
