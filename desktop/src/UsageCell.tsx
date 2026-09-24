import { useRef, useState } from "react";
import { LoaderCircle, RefreshCw, RotateCw } from "lucide-react";
import { api } from "./bridge";
import {
  fullTime,
  type Account,
  type UsageAction,
  type UsageStats,
} from "./types";

export function compactNumber(value: number): string {
  if (value >= 1e9) return `${(value / 1e9).toFixed(1).replace(/\.0$/, "")}B`;
  if (value >= 1e6) return `${(value / 1e6).toFixed(1).replace(/\.0$/, "")}M`;
  if (value >= 1e3) return `${(value / 1e3).toFixed(1).replace(/\.0$/, "")}K`;
  return String(value);
}
export function usageColor(value: number): string {
  return value >= 90 ? "red" : value >= 75 ? "amber" : "green";
}
function Stats({
  stats,
  estimated,
}: {
  stats: UsageStats;
  estimated?: number | null;
}) {
  return (
    <div className="usage-stats">
      <span title="请求数">{compactNumber(stats.requests)} req</span>
      <span title="Token（含缓存）">{compactNumber(stats.tokens)}</span>
      <span title="账号费用">A ${stats.cost.toFixed(2)}</span>
      <span title="用户费用">U ${stats.user_cost.toFixed(2)}</span>
      {estimated != null && <span>预计总费用 ${estimated.toFixed(2)}</span>}
    </div>
  );
}
const labels: Record<UsageAction, string> = {
  query_usage: "查询",
  query_reset_credits: "次数",
  reset_quota: "重置",
  probe_quota: "探测",
};
export default function UsageCell({
  account,
  online,
  refresh,
  report,
}: {
  account: Account;
  online: boolean;
  refresh: () => Promise<unknown>;
  report: (message: unknown) => void;
}) {
  const [busy, setBusy] = useState<UsageAction | null>(null);
  const running = useRef(false);
  const [confirm, setConfirm] = useState<{
    action: UsageAction;
    version: string;
  } | null>(null);
  const usage = account.usage;
  const windows = usage?.windows ?? account.usage_windows ?? [];
  async function run(
    action: UsageAction,
    version = account.version,
    confirmed = false,
  ) {
    if (running.current || !online) return;
    if (!confirmed && (action === "reset_quota" || action === "probe_quota")) {
      setConfirm({ action, version });
      return;
    }
    running.current = true;
    setBusy(action);
    setConfirm(null);
    try {
      const result = await api<{ message: string }>(
        "POST",
        `/accounts/${account.id}/usage-action`,
        { action, expected_version: version, confirmed },
      );
      report(result.message);
    } catch (error) {
      report(error);
    } finally {
      // Refresh is read-only, including after an uncertain write. Never retry the action.
      await refresh().catch(report);
      running.current = false;
      setBusy(null);
    }
  }
  return (
    <div className="usage-cell">
      {usage?.branch === "apikey" &&
        (usage.today ? (
          <Stats stats={usage.today} />
        ) : (
          <span className="usage-unknown">今日统计未知</span>
        ))}
      {windows.map((w) => {
        const percent = w.used_percent;
        const status = {
          known: "",
          unknown: "未知",
          stale: "历史",
          error: "异常",
        }[w.status];
        const tooltip = [
          status,
          `采集 ${fullTime(w.observed_at)}`,
          w.reset_at ? `重置 ${fullTime(w.reset_at)}` : "",
          w.key === "grok_24h" && w.limit
            ? `24h Token 上限 ${compactNumber(w.limit)}`
            : "",
        ]
          .filter(Boolean)
          .join(" · ");
        return (
          <div
            className={`usage-window ${w.status}`}
            key={w.key}
            title={tooltip}
          >
            {w.stats && (w.stats.requests > 0 || w.stats.tokens > 0) && (
              <Stats stats={w.stats} estimated={w.estimated_total_cost} />
            )}
            <div className="usage-progress">
              <span
                className={`usage-badge ${w.color ?? (w.label === "7d" ? "emerald" : "indigo")}`}
              >
                {w.label}
              </span>
              <div
                className="usage-track"
                role="progressbar"
                aria-label={`${w.label}用量`}
                aria-valuenow={percent ?? undefined}
                aria-valuemin={0}
                aria-valuemax={Math.max(100, percent ?? 100)}
              >
                {percent != null && (
                  <i
                    className={usageColor(percent)}
                    style={{ width: `${Math.min(100, percent)}%` }}
                  />
                )}
              </div>
              <span
                className={`usage-percent ${percent != null && percent >= 90 ? "red" : ""}`}
              >
                {percent == null
                  ? "—"
                  : percent > 999
                    ? ">999%"
                    : `${Math.round(percent)}%`}
              </span>
              {w.reset_at && <time>{fullTime(w.reset_at)}</time>}
              {status && <span className="usage-state">{status}</span>}
            </div>
          </div>
        );
      })}
      {(usage?.prepaid_balance ?? 0) > 0 && (
        <span className="usage-prepaid">
          预付 ${usage!.prepaid_balance!.toFixed(2)}
        </span>
      )}
      {(usage?.monthly_limit ?? 0) > 0 && (
        <span className="usage-money">
          已用{" "}
          {usage?.monthly_used == null
            ? "未知"
            : `$${usage.monthly_used.toFixed(2)}`}{" "}
          / ${usage!.monthly_limit!.toFixed(2)}
        </span>
      )}
      {!windows.length &&
        usage?.branch !== "apikey" &&
        !usage?.prepaid_balance && <span className="usage-unknown">未知</span>}
      {!!usage?.actions.length && (
        <div className="usage-actions">
          {usage.actions.map((action) => (
            <button
              key={action}
              className={`usage-action ${action}`}
              disabled={
                !online ||
                busy !== null ||
                (action === "reset_quota" && !usage.reset_credits?.available)
              }
              onClick={() => void run(action)}
            >
              {busy === action ? (
                <LoaderCircle className="spin" size={11} />
              ) : action === "reset_quota" ? (
                <RotateCw size={11} />
              ) : (
                <RefreshCw size={11} />
              )}
              {labels[action]}
              {action === "query_reset_credits"
                ? ` ${usage.reset_credits?.available ?? "—"}`
                : ""}
            </button>
          ))}
        </div>
      )}
      {!!usage?.reset_credits?.expires_at.length && (
        <div
          className="usage-expiry"
          title={`次数采集 ${fullTime(usage.reset_credits.observed_at)}`}
        >
          {usage.reset_credits.expires_at.map((at) => (
            <span key={at}>到期 {fullTime(at)}</span>
          ))}
        </div>
      )}
      {confirm && (
        <div
          className="modal-backdrop"
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.stopPropagation();
              setConfirm(null);
            }
          }}
        >
          <section
            className="modal"
            role="alertdialog"
            aria-label={
              confirm.action === "reset_quota"
                ? "确认额度重置"
                : "确认 Grok 探测"
            }
          >
            <h2>
              {confirm.action === "reset_quota"
                ? "确认额度重置？"
                : "确认 Grok 探测？"}
            </h2>
            <p>
              {confirm.action === "reset_quota"
                ? `「${account.name}」将消耗一次重置次数，并执行 Sub2API 的额度重置。`
                : `「${account.name}」的探测可能发送模型请求并产生用量。`}
            </p>
            <footer>
              <button autoFocus onClick={() => setConfirm(null)}>
                取消
              </button>
              <button
                className="primary"
                disabled={!online}
                onClick={() => void run(confirm.action, confirm.version, true)}
              >
                确认{labels[confirm.action]}
              </button>
            </footer>
          </section>
        </div>
      )}
    </div>
  );
}
