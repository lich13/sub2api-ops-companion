import { useEffect, useState } from "react";
import { CircleHelp, TriangleAlert } from "lucide-react";
import { fullTime, type Account } from "./types";

export function remainingRestriction(until: string | undefined, now: number): string {
  const end = until ? Date.parse(until) : NaN;
  if (!Number.isFinite(end)) return "时间未知";
  const seconds = Math.ceil((end - now) / 1000);
  if (seconds <= 0) return "等待状态刷新";
  if (seconds >= 86400) return `${Math.floor(seconds / 86400)}天${Math.floor(seconds % 86400 / 3600)}小时`;
  if (seconds >= 3600) return `${Math.floor(seconds / 3600)}小时${Math.floor(seconds % 3600 / 60)}分钟`;
  if (seconds >= 60) return `${Math.floor(seconds / 60)}分钟`;
  return `${seconds}秒`;
}

export default function AccountStatus({ account, onError }: {
  account: Account;
  onError?: (id: number) => void;
}) {
  const limit = account.blockers.find(blocker => blocker.code === "rate_limit_reset_at");
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const end = limit?.until ? Date.parse(limit.until) : NaN;
    if (!Number.isFinite(end)) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const update = () => {
      clearTimeout(timer);
      if (document.visibilityState === "hidden") return;
      const at = Date.now();
      setNow(at);
      const seconds = Math.ceil((end - at) / 1000);
      if (!Number.isFinite(seconds) || seconds <= 0) return;
      const unit = seconds >= 86400 ? 3600 : seconds >= 60 ? 60 : 1;
      const next = Math.floor(seconds / unit) * unit - 1;
      timer = setTimeout(update, Math.max(1, end - at - next * 1000));
    };
    update();
    document.addEventListener("visibilitychange", update);
    window.addEventListener("focus", update);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", update);
      window.removeEventListener("focus", update);
    };
  }, [limit?.until]);
  const label = account.available ? "可调度" : account.blockers.map(blocker => blocker.label).join(" / ") || "暂不可调度";
  const recentRateError = !!limit && !account.available && !account.success_after_error && account.last_error_status === 429;
  const reason = [label, recentRateError && `最近上游错误：HTTP 429 · ${fullTime(account.last_error_at)}`,
    recentRateError && account.error_message].filter(Boolean).join("；");
  const remaining = remainingRestriction(limit?.until, now);
  return <div className="account-status-detail">
    <div className="account-status-line">
      <span className={`account-status ${account.available ? "good-text" : ""} ${limit ? "pill warning" : ""}`}>
        {!limit && <i className={`status-dot ${account.available ? "good" : "warning"}`}/>}{label}
      </span>
      {limit && <span className="account-status-help" tabIndex={0} role="img" aria-label={`状态说明：${reason}`} title={reason}><CircleHelp size={13}/></span>}
      {recentRateError && (onError && account.last_error_id ?
        <button type="button" className="account-status-code code warning" title={reason} aria-label="查看最近上游错误 HTTP 429" onClick={() => onError(account.last_error_id!)}><TriangleAlert size={12}/>429</button> :
        <span className="account-status-code code warning" title={reason} aria-label="最近上游错误 HTTP 429"><TriangleAlert size={12}/>429</span>)}
    </div>
    {limit && <small className="limit-until" title={`预计解除 ${fullTime(limit.until)}`}>
      {remaining === "时间未知" || remaining === "等待状态刷新" ? remaining : `${remaining}后自动解除`}
    </small>}
  </div>;
}
