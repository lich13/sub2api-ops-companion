import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Play, Square, X } from "lucide-react";
import { api, command, runTest } from "./bridge";
import { fullTime, type Account, type TestEvent } from "./types";

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
export default function TestDialog({ account, online, close }: {account: Account; online: boolean; close: () => void}) {
  const [models, setModels] = useState<TestModel[]>([]);
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<TestMode>(account.platform === "grok" ? "text" : "default");
  const [model, setModel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [media, setMedia] = useState("");
  const [fileName, setFileName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [text, setText] = useState("");
  const [status, setStatus] = useState("");
  const [outputs, setOutputs] = useState<TestEvent[]>([]);
  const [result, setResult] = useState<TestEvent | null>(null);
  const running = useRef(false), alive = useRef(true), cancelled = useRef(false);
  useEffect(() => {
    alive.current = true;
    void api<TestModel[]>("GET", `/accounts/${account.id}/models`).then((list) => {
      if (!alive.current) return;
      setModels(list); setModel(preferredModel(list, account.platform, mode));
    }).catch((e) => { if (alive.current) setError(String(e)); }).finally(() => { if (alive.current) setLoading(false); });
    return () => { alive.current = false; if (running.current) void command("cancel_test"); };
  }, [account.id]);
  const choices: {id: TestMode; label: string}[] = account.platform === "grok"
    ? [{id:"text",label:"文本"},{id:"image",label:"图像"},{id:"video",label:"视频"},{id:"search",label:"搜索"},{id:"tts",label:"TTS"},{id:"stt",label:"STT"},{id:"realtime",label:"Realtime"}]
    : [{id:"default",label:"普通"},{id:"compact",label:"Compact"}];
  const available = modelsForMode(models, account.platform, mode);
  const standalone = ["search", "tts", "stt", "realtime"].includes(mode);
  const imageModel = account.platform === "openai" && mode === "default" && model.startsWith("gpt-image-");
  const hasPrompt = ["image", "video", "search", "tts"].includes(mode) || imageModel;
  function changeMode(next: TestMode) {
    setMode(next); setModel(preferredModel(models, account.platform, next)); setPrompt(defaults[next] ?? ""); setMedia(""); setFileName("");
  }
  async function chooseFile(file: File | undefined) {
    setMedia(""); setFileName(""); if (!file) return;
    const audio = mode === "stt";
    if (!file.type.startsWith(audio ? "audio/" : "image/")) { setError("文件格式不支持"); return; }
    if (file.size > (audio ? 6 : 4) * 1024 * 1024) { setError(audio ? "音频不得超过 6 MiB" : "图片不得超过 4 MiB"); return; }
    const reader = new FileReader();
    reader.onload = () => { if (alive.current) { setMedia(String(reader.result)); setFileName(file.name); setError(""); } };
    reader.onerror = () => setError("素材读取失败"); reader.readAsDataURL(file);
  }
  async function cancel() { cancelled.current = true; await command("cancel_test"); }
  async function start() {
    if (running.current || !online) return;
    running.current = true; cancelled.current = false; setBusy(true); setText(""); setError(""); setOutputs([]); setResult(null); setStatus("正在连接");
    let terminal = false;
    try {
      await runTest(account.id, {expected_version: account.version, mode, model_id: standalone ? "" : model,
        prompt: hasPrompt ? prompt || (imageModel ? defaults.image : defaults[mode]) || "" : "",
        ...(media ? {[mode === "stt" ? "audio_data_url" : "image_data_url"]: media} : {})}, (event) => {
        if (!alive.current || cancelled.current) return;
        if (event.type === "content") setText((old) => (old + (event.text ?? "")).slice(-500_000));
        if (event.type === "status") setStatus(event.text ?? "");
        if (event.type === "test_start") setStatus(event.model ?? "测试中");
        if (["image", "audio", "video"].includes(event.type)) setOutputs((old) => [...old, event]);
        if (event.type === "error" || event.type === "test_complete") { terminal = true; setResult(event); setStatus(event.type === "test_complete" && event.success ? "测试通过" : "测试失败"); if (event.error) setError(event.error); }
      });
      if (!terminal && !cancelled.current && alive.current) setError("连接结束，未收到完成结果");
    } catch (e) { if (alive.current && !cancelled.current) setError(String(e).replace(/^Error: /, "")); }
    finally {
      running.current = false;
      if (alive.current) { setBusy(false); if (cancelled.current) setStatus("已取消，未重放请求"); }
      void command("refresh");
    }
  }
  return <div className="modal-backdrop" onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } }}>
    <section className="test-dialog" role="dialog" aria-modal="true" aria-label="测试连接">
      <header><div><h2>测试连接</h2><strong title={account.name}>{account.name}</strong></div><button className="icon-button" title="关闭测试" onClick={close}><X size={18}/></button></header>
      <div className="test-controls">
        <label>模式<select disabled={busy} value={mode} onChange={(e) => changeMode(e.target.value as TestMode)}>{choices.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}</select></label>
        {!standalone && <label>模型<select aria-label="测试模型" disabled={busy || loading} value={model} onChange={(e) => { setModel(e.target.value); }}><option value="">默认模型</option>{available.map((m) => <option key={m.id} value={m.id}>{m.display_name || m.id}</option>)}</select></label>}
        {hasPrompt && <label className="test-prompt">提示词<textarea value={prompt} placeholder={imageModel ? defaults.image : defaults[mode]} maxLength={16000} disabled={busy} onChange={(e) => setPrompt(e.target.value)}/></label>}
        {((account.platform === "grok" && ["image", "video", "stt"].includes(mode)) || imageModel) && <label className="test-upload">{mode === "stt" ? "音频素材（可选）" : "图片素材（可选）"}<input key={`${mode}-${imageModel}`} type="file" accept={mode === "stt" ? "audio/*,.wav,.mp3,.m4a,.ogg,.webm" : "image/png,image/jpeg,image/webp,image/gif"} disabled={busy} onChange={(e) => void chooseFile(e.target.files?.[0])}/>{fileName && <span>{fileName}</span>}</label>}
      </div>
      <div className="test-result" aria-live="polite">
        {status && <div className="test-status">{busy && <LoaderCircle size={14} className="spin"/>}{status}{result?.duration_ms != null && <span>{(result.duration_ms / 1000).toFixed(2)} s · {fullTime(result.completed_at)}</span>}</div>}
        {text && <pre>{text}</pre>}
        {outputs.map((event, i) => <div className="test-media" key={i}>{event.image_url && <img src={event.image_url} alt="测试生成图片"/>}{event.audio_url && <audio src={event.audio_url} controls/>}{event.video_url && <video src={event.video_url} controls/>}</div>)}
        {error && <p className="bad-text" role="alert">{error}</p>}
      </div>
      <footer>{busy ? <button onClick={() => void cancel()}><Square size={13}/>取消</button> : <button disabled={!online || loading} onClick={() => void start()}><Play size={13}/>开始测试</button>}</footer>
    </section>
  </div>;
}
