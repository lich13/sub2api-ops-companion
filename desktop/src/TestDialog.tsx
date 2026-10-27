import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { LoaderCircle, RefreshCw, Play, Square, X } from "lucide-react";
import { api, command, runTest } from "./bridge";
import { fullTime, type Account, type TestEvent, type UsageSummary, type UsageWindow } from "./types";
import { submitOperation, followOperation, subscribeOperations, getOperations, setOperations, publishOperation, repeating, operationPending, type Operation } from "./accountOperations";
import { readAccountModels, modelDirectoryRevoked } from "./accountModels";
import { useBackAction } from "./mobile";

export type TestMode = "default" | "compact" | "text" | "image" | "video" | "search" | "tts" | "stt" | "realtime";
export type TestModel = {id: string; display_name: string; type: string};
const defaults: Partial<Record<TestMode, string>> = {
  image: "Generate a cute orange cat astronaut sticker on a clean pastel background.",
  video: "A red ball bouncing once on a white floor, short simple motion.",
  search: "xAI Grok", tts: "Hello from Sub2API account connectivity test.",
};
export function modelsForMode(models: TestModel[], platform: string, mode: TestMode) {
  if (platform !== "grok") return models;
  if (["search", "tts", "stt", "realtime"].includes(mode)) return [];
  const image = (id: string) => id === "grok-imagine" || id === "grok-imagine-edit" || id.startsWith("grok-imagine-image");
  const video = (id: string) => id.startsWith("grok-imagine-video") || id.startsWith("grok-video");
  return models.filter((m) => mode === "image" ? image(m.id) : mode === "video" ? video(m.id) : !image(m.id) && !video(m.id));
}
export function preferredModel(models: TestModel[], platform: string, mode: TestMode) {
  const options = modelsForMode(models, platform, mode);
  return (platform === "grok" ? options.find((m) => m.id.includes("grok-4.5")) ?? options.find((m) => m.id === "grok") : options.find((m) => m.id.includes("sonnet")))?.id ?? options[0]?.id ?? "";
}
export function nextBeijingMinute(now = Date.now()) {
  return new Date((Math.floor(now / 60_000) + 1) * 60_000 + 8 * 3600_000).toISOString().slice(0, 16);
}
export function appointmentTimestamp(value: string) {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::[0-5]\d(?:\.0{1,3})?)?$/.test(value)) return NaN;
  const second = value.length >= 19 ? value.slice(0, 19) : `${value}:00`, at = Date.parse(`${second}+08:00`);
  return Number.isFinite(at) && new Date(at + 8 * 3600_000).toISOString().slice(0, 19) === second ? at : NaN;
}
export function nextCycleAppointment(usage: UsageSummary | undefined, windows: UsageWindow[] = [], now = Date.now()) {
  const quota = usage?.key_quota?.state === "ok" ? usage.key_quota : undefined;
  const resets = [
    ...(usage?.windows ?? windows).filter(window => window.status !== "error" && window.status !== "unknown").map(window => window.reset_at),
    ...(quota?.plans ?? []).map(plan => plan.reset_at),
    ...(quota?.subscriptions ?? []).filter(subscription => subscription.active !== false && (!subscription.expires_at || Date.parse(subscription.expires_at) > now))
      .flatMap(subscription => subscription.plans.map(plan => plan.reset_at)),
  ].map(reset => reset ? Math.ceil(Date.parse(reset) / 1000) * 1000 + 3000 : NaN).filter(at => Number.isFinite(at) && at > now);
  return resets.length ? Math.min(...resets) : null;
}
export default function TestDialog({ account, online, close }: {account: Account; online: boolean; close: () => void}) {
  useBackAction(true, dismiss);
  const operations = useSyncExternalStore(subscribeOperations, getOperations);
  const scheduled = operations.filter(job => job.account_id === account.id && (job.scheduled_at || job.repeat) && operationPending(job));
  const [execution, setExecution] = useState("now");
  const continuous = execution === "cycle";
  const appointment = execution !== "now";
  const existingCycle = scheduled.some(repeating);
  const [scheduledAt, setScheduledAt] = useState("");
  const directoryRequest = useRef(0);
  const [modelError, setModelError] = useState("");
  const [models, setModels] = useState<TestModel[]>([]);
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<TestMode>(account.platform === "grok" ? "text" : "default");
  const [model, setModel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [media, setMedia] = useState("");
  const [fileName, setFileName] = useState("");
  const [mediaState, setMediaState] = useState<"" | "reading" | "failed">("");
  const mediaReader = useRef<FileReader | null>(null);
  const [busy, setBusy] = useState(false);
  const [cycleBusy, setCycleBusy] = useState(false);
  const cycleReading = useRef(false);
  const [error, setError] = useState("");
  const [text, setText] = useState("");
  const [status, setStatus] = useState("");
  const [outputs, setOutputs] = useState<TestEvent[]>([]);
  const [result, setResult] = useState<TestEvent | null>(null);
  const operationId = useRef<string | null>(null);
  const running = useRef(false), alive = useRef(true), cancelled = useRef(false);
  useEffect(() => {
    alive.current = true;
    if (account.operation_versions?.test) void api<{ items: Operation[] }>("GET", "/account-operations").then(data => { if (alive.current) setOperations(data.items); }).catch(e => { if (alive.current) setError(String(e)); });
    void readModels();
    return () => { ++directoryRequest.current; alive.current = false; stopMediaRead(); if (running.current && !account.operation_versions) void command("cancel_test"); };
  }, [account.id]);
  async function readModels(refresh = false) {
    const request = ++directoryRequest.current; setLoading(true); setModelError("");
    try { const list = await readAccountModels<TestModel>(account.id, refresh); if (alive.current && request === directoryRequest.current) setModels(list); }
    catch (error) { if (alive.current && request === directoryRequest.current) { if (modelDirectoryRevoked(error)) setModels([]); setModelError(`模型列表读取失败：${String(error).replace(/^Error: /, "")}`); } }
    finally { if (alive.current && request === directoryRequest.current) setLoading(false); }
  }
  const choices: {id: TestMode; label: string}[] = account.platform === "grok"
    ? [{id:"text",label:"文本"},{id:"image",label:"图像"},{id:"video",label:"视频"},{id:"search",label:"搜索"},{id:"tts",label:"TTS"},{id:"stt",label:"STT"},{id:"realtime",label:"Realtime"}]
    : account.platform === "openai" ? [{id:"default",label:"普通"},{id:"compact",label:"Compact"}] : [{id:"default",label:"普通"}];
  const available = modelsForMode(models, account.platform, mode);
  const standalone = ["search", "tts", "stt", "realtime"].includes(mode);
  const imageModel = account.platform === "openai" && mode === "default" && model.startsWith("gpt-image-");
  const hasPrompt = ["image", "video", "search", "tts"].includes(mode) || imageModel;
  function stopMediaRead() {
    const reader = mediaReader.current; mediaReader.current = null;
    if (reader?.readyState === FileReader.LOADING) reader.abort();
  }
  function clearMedia() { stopMediaRead(); setMedia(""); setFileName(""); setMediaState(""); }
  function dismiss() { clearMedia(); close(); }
  useEffect(() => { clearMedia(); }, [mode, imageModel, account.id]);
  function changeMode(next: TestMode) {
    clearMedia(); setMode(next); setModel(preferredModel(models, account.platform, next)); setPrompt(defaults[next] ?? "");
  }
  async function chooseFile(file: File | undefined) {
    clearMedia(); setError(""); if (!file) return;
    const audio = mode === "stt";
    if (!file.type.startsWith(audio ? "audio/" : "image/")) { setMediaState("failed"); setError("文件格式不支持"); return; }
    if (file.size > (audio ? 6 : 4) * 1024 * 1024) { setMediaState("failed"); setError(audio ? "音频不得超过 6 MiB" : "图片不得超过 4 MiB"); return; }
    const reader = new FileReader();
    mediaReader.current = reader; setMediaState("reading");
    const current = () => alive.current && mediaReader.current === reader;
    const failed = () => { if (current()) { mediaReader.current = null; setMediaState("failed"); setError("素材读取失败，请重新选择文件"); } };
    reader.onload = () => { if (current()) { mediaReader.current = null; setMedia(String(reader.result)); setFileName(file.name); setMediaState(""); setError(""); } };
    reader.onerror = failed; reader.onabort = failed;
    try { reader.readAsDataURL(file); } catch { failed(); }
  }
  async function cancel() { cancelled.current = true; if (operationId.current) await api("POST", `/account-operations/${operationId.current}/cancel`, {}); else await command("cancel_test"); }
  async function fillCycleTime() {
    if (cycleReading.current || running.current || !online) return;
    cycleReading.current = true; setCycleBusy(true); setError("");
    try {
      const usage = account.usage?.native_read ? await api<UsageSummary>("GET", `/accounts/${account.id}/usage`) : account.usage;
      if (!alive.current) return;
      const at = nextCycleAppointment(usage, account.usage_windows);
      if (at == null) { setError("暂无可用的周期重置时间，请手动设置预约时间。"); return; }
      setScheduledAt(new Date(at + 8 * 3600_000).toISOString().slice(0, 19));
    } catch (e) { if (alive.current) setError(`周期时间读取失败：${String(e).replace(/^Error: /, "")}`); }
    finally { cycleReading.current = false; if (alive.current) setCycleBusy(false); }
  }
  async function start() {
    if (running.current || cycleReading.current || mediaReader.current || mediaState || !online) return;
    if (appointment) {
      const scheduleTimestamp = appointmentTimestamp(scheduledAt);
      if (!continuous && !scheduledAt) { setError("请填写预约时间。"); return; }
      if (!continuous && !Number.isFinite(scheduleTimestamp)) { setError("预约时间无效，请选择有效日期并精确到秒。"); return; }
      if (!continuous && scheduleTimestamp <= Date.now()) { setError("预约时间须晚于当前北京时间。"); return; }
      if (continuous && existingCycle) { setError("此账号已有连续预约，请先停止现有预约。"); return; }
      running.current = true; setBusy(true); setError("");
      try {
        const job = await submitOperation(account, "test", { confirmed: true, mode: account.platform === "grok" ? "text" : "default", model_id: model, prompt: "" }, undefined, continuous ? undefined : new Date(scheduleTimestamp).toISOString(), continuous ? "quota_cycle" : undefined);
        if (alive.current) {
          if (job.status === "needs_confirmation" || job.repeat_state === "needs_confirmation" || job.status === "failed") setError(job.repeat_reason || job.reason || "预约需确认，请核对后重新提交。");
          else close();
        }
      } catch (e) { if (alive.current) setError(String(e).replace(/^Error: /, "")); }
      finally { running.current = false; if (alive.current) setBusy(false); }
      return;
    }
    running.current = true; cancelled.current = false; setBusy(true); setText(""); setError(""); setOutputs([]); setResult(null); setStatus("正在连接");
    let terminal = false;
    try {
      const body = {expected_version: account.version, mode, model_id: standalone ? "" : model,
        prompt: hasPrompt ? prompt || (imageModel ? defaults.image : defaults[mode]) || "" : "",
        ...(media ? {[mode === "stt" ? "audio_data_url" : "image_data_url"]: media} : {})};
      const receive = (event: TestEvent) => {
        if (!alive.current || cancelled.current) return;
        if (event.type === "content") setText((old) => (old + (event.text ?? "")).slice(-500_000));
        if (event.type === "status") setStatus(event.text ?? "");
        if (event.type === "test_start") setStatus(event.model ?? "测试中");
        if (["image", "audio", "video"].includes(event.type)) setOutputs((old) => [...old, event]);
        if (event.type === "error" || event.type === "test_complete") { terminal = true; setResult(event); setStatus(event.type === "test_complete" && event.success ? "测试通过" : "测试失败"); if (event.error) setError(event.error); }
      };
      if (account.operation_versions?.test) {
        const { expected_version: _version, ...payload } = body;
        const job = await submitOperation(account, 'test', { ...payload, confirmed: true });
        operationId.current = job.id;
        await followOperation(job, receive);
      } else await runTest(account.id, body, receive);
      if (!terminal && !cancelled.current && alive.current) setError("连接结束，未收到完成结果");
    } catch (e) { if (alive.current && !cancelled.current) setError(String(e).replace(/^Error: /, "")); }
    finally {
      running.current = false;
      if (alive.current) { setBusy(false); if (cancelled.current) setStatus("已取消，未重放请求"); }
      void command("refresh");
    }
  }
  return <div className="modal-backdrop" onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); dismiss(); } }}>
    <section className="test-dialog" role="dialog" aria-modal="true" aria-label="测试连接">
      <header><div><h2>测试连接</h2><strong title={account.name}>{account.name}</strong></div><button className="icon-button" title="关闭测试" onClick={dismiss}><X size={18}/></button></header>
      <div className="test-controls">
        {account.operation_versions?.test && <label>执行<select aria-label="执行方式" disabled={busy || cycleBusy} value={execution} onChange={e => { setExecution(e.target.value); if (e.target.value !== "now") { setScheduledAt(previous => previous || nextBeijingMinute()); if (mode !== (account.platform === "grok" ? "text" : "default")) changeMode(account.platform === "grok" ? "text" : "default"); } }}><option value="now">立即</option><option value="scheduled">指定时间</option>{account.platform === "openai" && account.type === "oauth" && <option value="cycle">持续预约 · 最近周期 +3 秒</option>}</select></label>}
        {execution === "scheduled" && <div className="test-schedule-time"><label>预约时间（北京时间）<input aria-label="预约时间" type="datetime-local" step={1} value={scheduledAt} disabled={busy || cycleBusy} onChange={e => setScheduledAt(e.target.value)}/></label><button type="button" disabled={busy || cycleBusy || !online} onClick={() => void fillCycleTime()}>{cycleBusy ? <LoaderCircle size={13} className="spin"/> : null}最近周期 +3 秒</button></div>}
        {continuous && <p className="muted">按最近额度周期重置后 3 秒持续测试，7d 额度满 100% 且上游 429 时停止。可在操作待办停止后续预约。</p>}
        <label>模式<select disabled={busy || appointment} value={mode} onChange={(e) => changeMode(e.target.value as TestMode)}>{choices.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}</select></label>
        {!standalone && <label>模型<span className="model-directory-control"><select aria-label="测试模型" disabled={busy || loading} value={model} onChange={(e) => { if (imageModel !== (account.platform === "openai" && mode === "default" && e.target.value.startsWith("gpt-image-"))) clearMedia(); setModel(e.target.value); }}><option value="">默认模型</option>{model && !available.some(item => item.id === model) && <option value={model}>{model}（当前不可用）</option>}{available.map((m) => <option key={m.id} value={m.id}>{m.display_name || m.id}</option>)}</select><button type="button" className="icon-button" aria-label="刷新模型列表" disabled={busy || loading || !online} onClick={() => void readModels(true)}><RefreshCw size={16} className={loading ? "spin" : undefined}/></button></span></label>}
        {hasPrompt && <label className="test-prompt">提示词<textarea value={prompt} placeholder={imageModel ? defaults.image : defaults[mode]} maxLength={16000} disabled={busy} onChange={(e) => setPrompt(e.target.value)}/></label>}
        {((account.platform === "grok" && ["image", "video", "stt"].includes(mode)) || imageModel) && <label className="test-upload">{mode === "stt" ? "音频素材（可选）" : "图片素材（可选）"}<input key={`${mode}-${imageModel}`} type="file" accept={mode === "stt" ? "audio/*,.wav,.mp3,.m4a,.ogg,.webm" : "image/png,image/jpeg,image/webp,image/gif"} disabled={busy} onChange={(e) => void chooseFile(e.target.files?.[0])}/>{fileName && <span>{fileName}</span>}</label>}
      </div>
      {scheduled.length > 0 && <ul className="test-schedules">{scheduled.map(job => <li key={job.id}><div><time>{fullTime(job.scheduled_at)}</time><span>{`${job.repeat ? "持续预约 · " : ""}${String(job.requested.model_id || "默认模型")}`}</span></div><span>{job.repeat_reason || ({ queued: "待执行", running: "测试中", checking: "核对中", needs_confirmation: "待核对", completed: "准备下一次预约" } as Record<string, string>)[job.status]}</span>{(job.status === "queued" || repeating(job)) && <button className="icon-button" aria-label={repeating(job) ? "停止连续预约" : `取消预约 ${fullTime(job.scheduled_at)}`} disabled={busy || !online} onClick={() => { void api<Operation>("POST", `/account-operations/${job.id}/cancel`, {}).then(result => { if (alive.current) publishOperation(result); }).catch(e => { if (alive.current) setError(String(e)); }); }}><X size={15}/></button>}</li>)}</ul>}
      <div className="test-result" aria-live="polite">
        {!online && <p className="bad-text">当前离线，请恢复连接后操作。</p>}
        {modelError && <p className="bad-text" role="alert">{modelError}</p>}
        {mediaState === "reading" && <p className="muted" role="status">正在读取素材</p>}
        {status && <div className="test-status">{busy && <LoaderCircle size={14} className="spin"/>}{status}{result?.duration_ms != null && <span>{(result.duration_ms / 1000).toFixed(2)} s · {fullTime(result.completed_at)}</span>}</div>}
        {text && <pre>{text}</pre>}
        {outputs.map((event, i) => <div className="test-media" key={i}>{event.image_url && <img src={event.image_url} alt="测试生成图片"/>}{event.audio_url && <audio src={event.audio_url} controls/>}{event.video_url && <video src={event.video_url} controls/>}</div>)}
        {error && <p className="bad-text" role="alert">{error}</p>}
      </div>
      <footer>{busy ? appointment ? <button disabled><LoaderCircle size={13} className="spin"/>正在提交预约</button> : <button onClick={() => void cancel()}><Square size={13}/>取消</button> : <button disabled={!online || cycleBusy || !!mediaState || (continuous && existingCycle)} onClick={() => void start()}><Play size={13}/>{appointment ? "预约测试" : "开始测试"}</button>}</footer>
    </section>
  </div>;
}
