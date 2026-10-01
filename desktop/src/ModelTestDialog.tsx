import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Play, RotateCcw, Square, X } from "lucide-react";
import { api } from "./bridge";
import type { Account } from "./types";
import { useBackAction } from "./mobile";

type Model = { id: string; display_name?: string; type?: string };
type Result = {
  id: string; account_id: number; account_name: string; requested_model: string;
  forwarded_model: string; returned_models: string[]; status: string;
  completed_groups: number; valid_groups: number; attempts: number;
  duration_ms: number; error?: string; report?: { prediction_name?: string; prediction?: string; probability?: number; used_outputs?: number } | null;
};
const terminal = new Set(["completed", "failed", "cancelled", "interrupted"]);

export default function ModelTestDialog({ account, online, close, report }: {
  account: Account; online: boolean; close: () => void; report: (error: unknown) => void;
}) {
  const [models, setModels] = useState<Model[]>([]), [model, setModel] = useState("");
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
      setModels(items); setModel((old) => old && items.some((item) => item.id === old) ? old : items[0]?.id ?? "");
    }).catch((e) => { if (alive) { setModels([]); setModel(""); setModelError(e instanceof Error ? e.message : "模型候选读取失败"); reportRef.current(e); } }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; ++generation.current; clearTimeout(timer.current); };
  }, [account.id, modelReload]);
  useEffect(() => {
    let alive = true;
    void api<Result | null>("GET", `/accounts/${account.id}/model-tests/latest`).then((value) => { if (alive && value) setJob(value); }).catch(() => {});
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
    setBusy(true);
    try {
      const next = await api<Result>("POST", `/accounts/${account.id}/model-tests`, {
        model_id: model, expected_version: account.version,
        request_id: `${crypto.randomUUID().replaceAll("-", "").slice(0, 32)}`,
      });
      setJob(next);
    } catch (e) { reportRef.current(e); } finally { setBusy(false); }
  }
  async function cancel() {
    if (!job || busy || terminal.has(job.status)) return;
    setBusy(true); try { setJob(await api<Result>("POST", `/model-tests/${job.id}/cancel`, {})); } catch (e) { reportRef.current(e); } finally { setBusy(false); }
  }
  const running = !!job && !terminal.has(job.status);
  const reportResult = job?.report;
  return <div className="modal-backdrop" onClick={close}><section className="model-test-dialog" role="dialog" aria-modal="true" aria-label="模型测试" onClick={(e) => e.stopPropagation()}>
    <header><div><h2>模型测试</h2><strong>{account.name} <span>#{account.id}</span></strong></div><button className="icon-button" aria-label="关闭模型测试" onClick={close}><X size={18}/></button></header>
    <label className="model-test-model">模型<select value={model} disabled={loading || !!modelError || !models.length || running || busy} onChange={(e) => setModel(e.target.value)}>{models.map((item) => <option key={item.id} value={item.id}>{item.display_name || item.id}</option>)}</select>
      {modelError ? <span className="bad-text" role="alert">{modelError} <button type="button" className="link-button" onClick={() => { setModelError(""); setLoading(true); setModelReload((value) => value + 1); }}>重试</button></span> : !loading && !models.length ? <span className="muted">暂无可用模型</span> : null}
    </label>
    {job && <div className="model-test-result" aria-live="polite">
      <div className="model-test-status">{job.status === "running" || job.status === "retrying" ? <LoaderCircle size={16} className="spin"/> : null}<span>{job.status === "completed" ? "已完成" : job.status === "cancelled" ? "已停止" : job.status === "interrupted" ? "服务重启后中断" : job.status === "failed" ? "测试失败" : `${job.completed_groups}/3 组`}</span><span>{job.attempts} 次请求</span></div>
      <dl><div><dt>请求模型</dt><dd>{job.requested_model}</dd></div><div><dt>转发模型</dt><dd>{job.forwarded_model}</dd></div><div><dt>返回模型</dt><dd>{job.returned_models.length ? job.returned_models.join("、") : "—"}</dd></div><div><dt>指纹推测</dt><dd>{reportResult?.prediction_name || "—"}</dd></div><div><dt>匹配度</dt><dd>{reportResult?.probability == null ? "—" : `${(reportResult.probability * 100).toFixed(1)}%`}</dd></div><div><dt>有效组数</dt><dd>{reportResult?.used_outputs ?? job.valid_groups}/3</dd></div><div><dt>耗时</dt><dd>{job.duration_ms ? `${(job.duration_ms / 1000).toFixed(1)}s` : "—"}</dd></div></dl>
      {job.error && <p className="bad-text">{job.error}</p>}
    </div>}
    <footer>{running ? <button className="danger-text" disabled={busy} onClick={() => void cancel()}><Square size={15}/>停止</button> : <><button disabled={loading || !!modelError || !models.length || busy || !model || !online} onClick={() => void start()}>{busy ? <LoaderCircle size={15} className="spin"/> : job ? <RotateCcw size={15}/> : <Play size={15}/>} {job ? "重新测试" : "开始测试"}</button></>}</footer>
  </section></div>;
}
