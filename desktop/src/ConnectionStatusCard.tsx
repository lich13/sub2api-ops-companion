import { useEffect, useRef, useState } from "react";
import { LoaderCircle, RefreshCw } from "lucide-react";
import { api } from "./bridge";
import { fullTime } from "./types";

type Status = { state: string; endpoint: string; http_status: number | null; message: string; retryable: boolean; checked_at: string | null };
const labels: Record<string, string> = { ok: "上游连接正常", network_unreachable: "上游不可达", auth_rejected: "管理员 Key 被拒绝，请重新连接", route_not_found: "管理接口路径不兼容", protocol_mismatch: "管理接口响应不兼容", upstream_error: "上游服务异常" };
export default function ConnectionStatusCard({ connectionKey }: { connectionKey: string }) {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState(""), [loading, setLoading] = useState(false), [revision, setRevision] = useState(0);
  const generation = useRef(0);
  useEffect(() => {
    const current = ++generation.current;
    setLoading(true); setStatus(null); setError("");
    void api<Status>("GET", "/connection-status").then((value) => {
      if (current === generation.current) setStatus(value);
    }).catch((e) => {
      if (current === generation.current) setError(e instanceof Error ? e.message : "连接状态读取失败");
    }).finally(() => { if (current === generation.current) setLoading(false); });
    return () => { ++generation.current; };
  }, [connectionKey, revision]);
  return <section className="settings-card connection-diagnostic" aria-label="连接诊断">
    <div className="connection-diagnostic-heading"><h2>连接诊断</h2><button disabled={loading} onClick={() => setRevision((value) => value + 1)}>{loading ? <LoaderCircle size={15} className="spin"/> : <RefreshCw size={15}/>}重新检查</button></div>
    {status && <div role="status"><strong className={status.state === "ok" ? "good-text" : "bad-text"}>{labels[status.state] || "连接状态未知"}</strong>{status.message && <p>{status.message}</p>}<span className="muted">{status.http_status ? `HTTP ${status.http_status} · ` : ""}{fullTime(status.checked_at)}</span></div>}
    {error && <p className="bad-text" role="alert">{error}</p>}
  </section>;
}
