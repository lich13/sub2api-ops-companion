import { useEffect, useRef, useState } from "react";
import { api, command } from "./bridge";

export type Sub2ApiUpdate = {
  current_version: string; latest_version: string; has_update: boolean;
  cached: boolean; build_type: string; warning?: string;
  release_info?: { name: string; html_url: string; published_at: string; body: string };
};
export type Sub2ApiUpdateResult = {
  message: string; need_restart?: boolean; already_up_to_date?: boolean;
  current_version?: string; latest_version?: string; operation_id?: string;
};
const normalize = (version: string) => version.replace(/^v/, "");

export default function Sub2ApiVersion({ active, online, knownVersion = "", loadingVersion = false, onRuntimeChanged }: {
  active: boolean; online: boolean; knownVersion?: string; loadingVersion?: boolean; onRuntimeChanged?: () => void;
}) {
  const [version, setVersion] = useState(""), [info, setInfo] = useState<Sub2ApiUpdate | null>(null);
  const [supported, setSupported] = useState<boolean | null>(null), [busy, setBusy] = useState(false);
  const [needRestart, setNeedRestart] = useState(false), [waiting, setWaiting] = useState(false);
  const [error, setError] = useState(""), [message, setMessage] = useState("");
  const alive = useRef(true), guard = useRef(false), target = useRef(""), nativeLoaded = useRef(false);
  const refreshRuntime = useRef(onRuntimeChanged);
  refreshRuntime.current = onRuntimeChanged;
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => {
    if (!online) { setSupported(null); nativeLoaded.current = false; return; }
    if (!active || supported !== null) return;
    let cancelled = false;
    void api<{ features?: { sub2api_update?: boolean } }>("GET", "/capabilities").then(caps => {
      if (cancelled) return;
      setSupported(caps.features?.sub2api_update === true);
    }).catch(e => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [active, online, supported]);
  useEffect(() => {
    if (loadingVersion) return;
    if (knownVersion) { setVersion(knownVersion); nativeLoaded.current = false; return; }
    if (!active || !online || supported !== true || nativeLoaded.current) return;
    let cancelled = false;
    void api<{ version: string }>("GET", "/sub2api-system/version")
      .then(current => { if (!cancelled) { nativeLoaded.current = true; setVersion(current.version); } })
      .catch(e => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [active, online, supported, knownVersion, loadingVersion]);
  useEffect(() => {
    if (!waiting) return;
    if (!active) { setWaiting(false); return; }
    let cancelled = false, attempts = 0, timer: ReturnType<typeof setTimeout>;
    async function check() {
      try {
        const current = await api<{ version: string }>("GET", "/sub2api-system/version");
        if (cancelled) return;
        setVersion(current.version);
        if (target.current && normalize(current.version) === normalize(target.current)) {
          setWaiting(false); setNeedRestart(false); setError(""); setMessage("更新已生效");
          setInfo(previous => previous ? { ...previous, current_version: current.version, has_update: false } : previous);
          target.current = "";
          refreshRuntime.current?.();
          return;
        }
      } catch { /* A restarting native service may briefly be unreachable. */ }
      if (cancelled) return;
      if (++attempts >= 20) { setWaiting(false); setMessage("尚未确认运行版本，请手动刷新"); return; }
      timer = setTimeout(() => void check(), 3000);
    }
    timer = setTimeout(() => void check(), 1500);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [waiting, active]);
  async function run(action: "check-updates" | "update" | "restart") {
    if (guard.current || !online) return;
    guard.current = true; setBusy(true); setError(""); setMessage("");
    try {
      if (action === "check-updates") {
        const result = await api<Sub2ApiUpdate>("GET", "/sub2api-system/check-updates?force=true");
        if (!alive.current) return;
        setInfo(result); setVersion(result.current_version);
        if (target.current && normalize(result.current_version) === normalize(target.current)) {
          target.current = ""; setNeedRestart(false); setWaiting(false);
        }
        if (version && normalize(result.current_version) !== normalize(version)) refreshRuntime.current?.();
      } else {
        const result = await api<Sub2ApiUpdateResult>("POST", `/sub2api-system/${action}`);
        if (!alive.current) return;
        if (action === "restart") { setWaiting(true); setMessage("正在等待重启生效"); }
        else if (result.need_restart) { target.current = info?.latest_version || ""; setNeedRestart(true); setMessage("更新已下载，重启后生效"); }
        else if (result.already_up_to_date) {
          if (version && normalize(result.current_version!) !== normalize(version)) refreshRuntime.current?.();
          setVersion(result.current_version!); setNeedRestart(false); setMessage("已经是最新版本");
          setInfo(previous => previous ? { ...previous, current_version: result.current_version!, latest_version: result.latest_version!, has_update: false } : previous);
        }
      }
    } catch (e) {
      if (alive.current) {
        setError(String(e));
        if (action === "restart") setWaiting(true);
      }
    } finally { guard.current = false; if (alive.current) setBusy(false); }
  }
  if (supported === false) return null;
  return <div className="config-version">
    <div className="config-runtime"><b>运行 {version || "—"}</b>
      {info && <span>{info.has_update ? `可更新至 ${info.latest_version}` : "已经是最新版本"}</span>}
      <button type="button" disabled={supported !== true || busy || waiting || !online} onClick={() => void run("check-updates")}>检查更新</button>
      <button type="button" onClick={() => void command("open_sub2api_release", { url: info?.release_info?.html_url || "https://github.com/Wei-Shaw/sub2api/releases" }).catch(e => setError(String(e)))}>查看发布</button>
      {info?.has_update && !needRestart && <button type="button" disabled={busy || waiting || !online || info.build_type !== "release"} onClick={() => void run("update")}>{busy ? "正在更新…" : "更新 Sub2API"}</button>}
      {needRestart && <button type="button" disabled={busy || waiting || !online} onClick={() => void run("restart")}>重启生效</button>}
    </div>
    {info?.build_type === "source" && <p className="muted">源码构建版本不支持在线更新</p>}
    {info?.warning && <p role="status">{info.warning}</p>}
    {message && <p role="status">{message}</p>}
    {error && <p role="alert" className="bad-text">{error}</p>}
  </div>;
}
