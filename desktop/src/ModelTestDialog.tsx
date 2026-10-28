import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Play, RefreshCw, RotateCcw, Square, X } from "lucide-react";
import { api } from "./bridge";
import { readAccountModels, modelDirectoryRevoked } from "./accountModels";
import type { Account } from "./types";
import FingerprintBankStatus from "./FingerprintBankStatus";
import { ModelFingerprintResult, TokenizerResult } from "./ModelFingerprintResult";
import { engineName, type DetectionEngine, type ModelTestPreference, type ModelTestResult as Result } from "./modelTestTypes";
import { useDialogFocus } from "./dialogFocus";
import { useBackAction } from "./mobile";

type Model = { id: string; display_name?: string; type?: string };
const terminal = new Set(["completed", "failed", "cancelled", "interrupted", "needs_confirmation"]);

export default function ModelTestDialog({ account, online, close, report, concurrency = 1, saveConcurrency, preference, savePreference }: {
  account: Account; online: boolean; close: () => void; report: (error: unknown) => void;
  concurrency?: number; saveConcurrency?: (value: number) => void;
  preference?: ModelTestPreference; savePreference?: (value: ModelTestPreference) => void;
}) {
  const [engine, setEngine] = useState<DetectionEngine>(account.platform !== "openai" || preference?.engine === "lm_detector" ? "lm_detector" : "modeltrace");
  const [samples, setSamples] = useState<1 | 3>(preference?.sample_count === 3 ? 3 : 1);
  const [tokenizer, setTokenizer] = useState(false);
  const [slots, setSlots] = useState([1, 2, 3].includes(concurrency) ? concurrency : 1);
  const [models, setModels] = useState<Model[]>([]), [model, setModel] = useState("");
  const refreshDirectory = useRef(false);
  const selectionTouched = useRef(false), submitted = useRef(false);
  const scope = useRef(0), activeAccount = useRef(account.id);
  const [job, setJob] = useState<Result | null>(null), [loading, setLoading] = useState(true);
  const [defaultsLoading, setDefaultsLoading] = useState(account.type === "apikey");
  const [modelError, setModelError] = useState(""), [modelReload, setModelReload] = useState(0);
  const [busy, setBusy] = useState(false), generation = useRef(0), timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const reportRef = useRef(report), dialogRef = useRef<HTMLElement>(null); reportRef.current = report;
  const dismiss = () => { ++scope.current; ++generation.current; clearTimeout(timer.current); close(); };
  useDialogFocus(dialogRef);
  useBackAction(true, dismiss);
  useEffect(() => {
    ++scope.current;
    if (activeAccount.current !== account.id) {
      activeAccount.current = account.id;
      setModels([]); setModel(""); setJob(null); setTokenizer(false);
      setEngine(account.platform !== "openai" || preference?.engine === "lm_detector" ? "lm_detector" : "modeltrace"); setSamples(preference?.sample_count === 3 ? 3 : 1);
      selectionTouched.current = false; submitted.current = false;
    }
    setBusy(false);
    return () => { ++scope.current; };
  }, [account.id, online]);
  useEffect(() => {
    let alive = true; const current = scope.current;
    if (!online) { setLoading(false); return; }
    setLoading(true); setModelError("");
    const refresh = refreshDirectory.current; refreshDirectory.current = false;
    void readAccountModels<Model>(account.id, refresh).then((items) => {
      if (!alive || current !== scope.current) return;
      setModels(items); setModel((old) => old || items[0]?.id || "");
    }).catch((e) => { if (alive && current === scope.current) { if (modelDirectoryRevoked(e)) setModels([]); setModelError(e instanceof Error ? e.message : "模型候选读取失败"); reportRef.current(e); } }).finally(() => { if (alive && current === scope.current) setLoading(false); });
    return () => { alive = false; };
  }, [account.id, online, modelReload]);
  useEffect(() => {
    let alive = true; const current = scope.current;
    if (!online) return;
    const keyAccount = account.type === "apikey";
    setDefaultsLoading(keyAccount);
    void Promise.allSettled([
      api<Result | null>("GET", `/accounts/${account.id}/model-tests/latest`),
      keyAccount ? api<{ model_override?: string | null }>("GET", `/accounts/${account.id}/model-detection`) : Promise.resolve(null),
    ]).then(([latest, detection]) => {
      if (!alive || current !== scope.current) return;
      const value = latest.status === "fulfilled" ? latest.value : null;
      const preferred = detection.status === "fulfilled" ? detection.value?.model_override : null;
      if (!submitted.current) {
        if (value) setJob(value);
        if (!selectionTouched.current) {
          if (value && !terminal.has(value.status)) { setModel(value.requested_model); setEngine(value.engine ?? "modeltrace"); setSamples(value.planned_groups === 3 ? 3 : 1); setSlots(value.concurrency ?? 1); }
          else if (preferred) setModel(preferred);
          else if (value) setModel(value.requested_model);
        }
      }
      setDefaultsLoading(false);
    });
    return () => { alive = false; };
  }, [account.id, online]);
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
  }, [job?.id, job?.status, account.id, online]);
  async function start() {
    if (busy || defaultsLoading || !online || !model || !models.some((item) => item.id === model)) return;
    const current = scope.current;
    submitted.current = true;
    setBusy(true);
    try {
      const next = await api<Result>("POST", `/accounts/${account.id}/model-tests`, {
        model_id: model, expected_version: account.version,
        ...(account.operation_versions?.model_test ? { expected_operation_version: account.operation_versions.model_test } : {}),
        concurrency: slots, engine, ...(engine === "lm_detector" ? { sample_count: samples, tokenizer_probe: tokenizer } : {}),
        request_id: `${crypto.randomUUID().replaceAll("-", "").slice(0, 32)}`,
      });
      if (current === scope.current) { setJob(next); savePreference?.({ engine, sample_count: samples }); }
    } catch (e) { if (current === scope.current) reportRef.current(e); } finally { if (current === scope.current) setBusy(false); }
  }
  async function cancel() {
    if (!job || busy || (terminal.has(job.status) && job.status !== "needs_confirmation")) return;
    const current = scope.current;
    setBusy(true); try { const next = await api<Result>("POST", `/model-tests/${job.id}/cancel`, {}); if (current === scope.current) setJob(next); } catch (e) { if (current === scope.current) reportRef.current(e); } finally { if (current === scope.current) setBusy(false); }
  }
  async function retryFailed() {
    if (!job || busy) return;
    const current = scope.current;
    setBusy(true);
    try { const next = await api<Result>("POST", `/model-tests/${job.id}/retry`, { request_id: crypto.randomUUID() }); if (current === scope.current) setJob(next); }
    catch (e) { if (current === scope.current) reportRef.current(e); } finally { if (current === scope.current) setBusy(false); }
  }
  const running = !!job && !terminal.has(job.status);
  const reportResult = job?.report;
  const available = models.some((item) => item.id === model);
  return <div className="modal-backdrop" onClick={dismiss}><section ref={dialogRef} className="model-test-dialog" role="dialog" aria-modal="true" aria-label="模型测试" tabIndex={-1} onClick={(e) => e.stopPropagation()} onKeyDown={(e) => { if (e.key === "Escape" && !e.nativeEvent.isComposing) { e.preventDefault(); e.stopPropagation(); dismiss(); } }}>
    <header><div><h2>模型测试</h2><strong>{account.name} <span>#{account.id}</span></strong></div><button className="icon-button" aria-label="关闭模型测试" onClick={dismiss}><X size={18}/></button></header>
    <div className="model-test-content">
    <div className="model-test-controls">
    <label className="model-test-model model-test-model-main">模型<span className="model-directory-control"><select aria-label="测试模型" value={model} disabled={loading || !models.length || running || busy || !online} onChange={(e) => { selectionTouched.current = true; setModel(e.target.value); }}>{model && !available && <option value={model}>{model}{!loading && !modelError && online ? "（当前不可用）" : ""}</option>}{models.map((item) => <option key={item.id} value={item.id}>{item.display_name || item.id}</option>)}</select><button type="button" className="icon-button" aria-label="刷新模型列表" disabled={loading || running || busy || !online} onClick={() => { refreshDirectory.current = true; setModelReload(value => value + 1); }}><RefreshCw size={16} className={loading ? "spin" : undefined}/></button></span>
      {modelError ? <span className="bad-text" role="alert">{modelError} <button type="button" className="link-button" onClick={() => { refreshDirectory.current = true; setModelError(""); setLoading(true); setModelReload((value) => value + 1); }}>重试</button></span> : !loading && !models.length ? <span className="muted">暂无可用模型</span> : null}
      {!loading && online && !modelError && model && !available && <span className="bad-text">所选模型不在当前账号上游模型目录中。</span>}
    </label>
    <label className="model-test-model">检测源<select aria-label="检测源" value={engine} disabled={running || busy || !online} onChange={e => setEngine(e.target.value as DetectionEngine)}>{account.platform === "openai" && <option value="modeltrace">ModelTrace</option>}<option value="lm_detector">LM Detector</option></select></label>
    <label className="model-test-model">组数<select aria-label="采样组数" value={engine === "lm_detector" ? samples : 3} disabled={engine !== "lm_detector" || running || busy || !online} onChange={e => setSamples(Number(e.target.value) as 1 | 3)}>{engine === "lm_detector" && <option value={1}>1 组</option>}<option value={3}>3 组</option></select></label>
    <label className="model-test-model">并发<select aria-label="测试并发" value={slots} disabled={running || busy} onChange={(e) => { const value = Number(e.target.value); setSlots(value); saveConcurrency?.(value); }}>{[1, 2, 3].map((value) => <option key={value} value={value}>{value}</option>)}</select></label>
    </div>
    {engine === "lm_detector" && <div className="lm-probe-option"><label><input type="checkbox" checked={tokenizer} disabled={running || busy || !online} onChange={e => setTokenizer(e.target.checked)}/>分词器探测</label><p className="muted">数字采样完成后执行，最多额外 22 次请求，每次最长 90 秒。</p></div>}
    {!online && <p className="muted" role="status">连接离线，恢复连接后可继续操作。</p>}
    {job && <div className="model-test-result" aria-live="polite">
      <div className="model-test-status">{job.status === "running" || job.status === "retrying" ? <LoaderCircle size={16} className="spin"/> : null}<span>{job.status === "queued" ? "已排队" : job.status === "needs_confirmation" ? "账号已变化，需重新确认" : job.status === "completed" ? job.completion_reason === "automatic_degradation" ? job.engine === "lm_detector" ? "检测完成 · 判定降智" : "首组命中降智模型" : job.completion_reason === "confidence_99" ? "已提前完成" : "已完成" : job.status === "cancelled" ? "已停止" : job.status === "interrupted" ? "服务重启后中断" : job.status === "failed" ? "测试失败" : job.phase === "tokenizer" ? "数字采样完成 · 分词器探测中" : `${job.completed_groups}/${job.planned_groups ?? 3} 组`}</span><span>{job.attempts} 次请求</span></div>
      <p className="model-test-source">{engineName(job.engine)} · {job.planned_groups ?? 3} 组</p>
      {job.engine === "lm_detector" && <ModelFingerprintResult key={job.id} job={job}/>}
      {job.groups && <details className="model-test-group-details" open={job.engine !== "lm_detector" || running}><summary>各组采样与预测</summary><div className="model-test-groups">{job.groups.map((group) => <div key={group.index}><strong>第 {group.index} 组</strong><span>{({ queued: "等待", running: "等待响应", receiving: "接收中", analyzing: "分析中", skipped: "提前结束", retrying: "等待重试", completed: "完成", failed: "失败", cancelled: "已停止" } as Record<string, string>)[group.status] ?? group.status}</span><span>{group.attempts} 次</span><span>首字 {group.ttft_ms == null ? "—" : `${(group.ttft_ms / 1000).toFixed(1)}s`}</span><span>耗时 {group.duration_ms == null ? "—" : `${(group.duration_ms / 1000).toFixed(1)}s`}</span>{group.report && <span className="model-test-group-prediction">{group.report.prediction_name || "无排名"}{group.report.merged_class ? " · 合并类别，无法区分" : ""}</span>}{group.error && <span className="bad-text">{group.error}</span>}</div>)}</div></details>}
      <details className="model-test-request-details" open={job.engine !== "lm_detector"}><summary>请求模型与耗时</summary><dl><div><dt>请求模型</dt><dd>{job.requested_model}</dd></div><div><dt>转发模型</dt><dd>{job.forwarded_model}</dd></div><div><dt>返回模型</dt><dd>{job.returned_models.length ? job.returned_models.join("、") : "—"}</dd></div><div><dt>指纹推测</dt><dd>{reportResult?.prediction_name || "—"}</dd></div><div><dt>匹配度</dt><dd>{reportResult?.probability == null ? "—" : `${(reportResult.probability * 100).toFixed(job?.engine === "lm_detector" ? 2 : 1)}%`}</dd></div><div><dt>有效组数</dt><dd>{reportResult?.used_outputs ?? job.valid_groups}/{job.planned_groups ?? 3}</dd></div><div><dt>耗时</dt><dd>{job.duration_ms ? `${(job.duration_ms / 1000).toFixed(1)}s` : "—"}</dd></div></dl></details>
      <TokenizerResult job={job}/>
      {job.groups?.some((group) => group.diagnostics) && <details className="model-test-diagnostics"><summary>请求诊断</summary>{job.groups.filter((group) => group.diagnostics).map((group) => <dl key={group.index}><div><dt>第 {group.index} 组</dt><dd>{group.diagnostics?.response_protocol || group.diagnostics?.protocol || '—'}</dd></div><div><dt>HTTP</dt><dd>{group.diagnostics?.http_status ?? '—'} · {group.diagnostics?.content_type || '—'}</dd></div><div><dt>首事件</dt><dd>{group.diagnostics?.first_event_type || '—'} · {group.diagnostics?.first_event_ms == null ? '—' : `${(group.diagnostics.first_event_ms / 1000).toFixed(2)}s`}</dd></div><div><dt>末事件</dt><dd>{group.diagnostics?.last_event_type || '—'}</dd></div><div><dt>输出 Token</dt><dd>{group.diagnostics?.output_tokens ?? '—'}{group.diagnostics?.max_output_tokens != null ? ` / ${group.diagnostics.max_output_tokens}` : ''}</dd></div><div><dt>思考 Token</dt><dd>{group.diagnostics?.reasoning_tokens ?? '—'}</dd></div><div><dt>接收</dt><dd>{group.diagnostics?.bytes ?? 0} bytes</dd></div><div><dt>结束</dt><dd>{group.diagnostics?.end_reason || '—'}</dd></div></dl>)}</details>}
      {job.automatic_disposition && <p>{job.automatic_disposition.basis === "sample_family" ? `第 ${job.automatic_disposition.group_indexes?.join("、")} 组跨家族证据` : job.automatic_disposition.basis === "aggregate" ? "最终汇总" : "单组"} · {job.automatic_disposition.kind === "recheck" ? job.automatic_disposition.reason : <>{job.automatic_disposition.marked ? "标记已保存" : "标记待核对"} · {job.automatic_disposition.schedule_verified ? "停调度已确认" : "停调度未确认"}{job.automatic_disposition.reason && `：${job.automatic_disposition.reason}`}</>}</p>}
      {job.error && <p className="bad-text">{job.error}</p>}
    </div>}
    {job?.bank_version && <details className="model-test-version"><summary>本轮库版本 · {engineName(job.engine)}</summary><p>{job.bank_version.revision} · {job.bank_version.analyzer_version}</p></details>}
    <FingerprintBankStatus key={engine} engine={engine} online={online} taskVersion={job?.engine === engine || (!job?.engine && engine === "modeltrace") ? job?.bank_version : undefined}/>
    </div>
    <footer>{job?.can_retry && !running && <button disabled={busy || !online} onClick={() => void retryFailed()}>重试失败组</button>}{job?.status === "needs_confirmation" && <button disabled={busy || !online} onClick={() => void cancel()}>取消任务</button>}{running ? <button className="danger-text" disabled={busy || !online} onClick={() => void cancel()}><Square size={15}/>停止</button> : <><button disabled={loading || defaultsLoading || !models.length || busy || !model || !available || !online} onClick={() => void start()}>{busy ? <LoaderCircle size={15} className="spin"/> : job ? <RotateCcw size={15}/> : <Play size={15}/>} {job ? "重新测试" : "开始测试"}</button></>}</footer>
  </section></div>;
}
