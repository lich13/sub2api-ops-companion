import { accountOperation } from "./accountOperations";
import { useEffect, useRef, useState } from "react";
import { LoaderCircle, RefreshCw, RotateCw } from "lucide-react";
import { api } from "./bridge";
import { useBackAction } from "./mobile";
import {
  fullTime,
  type Account,
  type UsageAction,
  type UsageStats,
  type UsageSummary,
  type QuotaBatch,
} from "./types";

export const usageRefreshEvent = "sub2ops-usage-refreshed";

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
  active = true,
  connectionKey = "",
}: {
  account: Account;
  online: boolean;
  refresh: () => Promise<unknown>;
  report: (message: unknown) => void;
  active?: boolean;
  connectionKey?: string;
}) {
  const [busy, setBusy] = useState<UsageAction | null>(null);
  const running = useRef(false);
  const [confirm, setConfirm] = useState<{
    action: UsageAction;
    version: string;
  } | null>(null);
  useBackAction(!!confirm, () => setConfirm(null));
  const [native, setNative] = useState<{ identity: string; value: UsageSummary } | null>(null);
  const lastRead = useRef({ identity: "", at: 0 });
  const request = useRef(0);
  const identity = `${connectionKey}:${account.id}:${account.operation_versions?.usage ?? account.version}`;
  const usage = native?.identity === identity ? { ...native.value, today: account.usage?.today ?? native.value.today } : account.usage;
  const acceptNative = (value: UsageSummary) => setNative(previous => {
    if (previous?.identity === identity && (value.status === "error" || value.status === "authorization_required")) {
      value = { ...value,
        windows: value.windows.some(w => w.source !== "configured_quota") ? value.windows : [...value.windows, ...previous.value.windows.filter(w => w.source !== "configured_quota").map(w => ({ ...w, status: "error" as const }))],
        balances: value.balances?.length ? value.balances : previous.value.balances?.map(b => ({ ...b, status: "error" as const })),
      };
    }
    return { identity, value };
  });
  useEffect(() => () => { ++request.current; lastRead.current = { identity: "", at: 0 }; }, [identity, online, active]);
  useEffect(() => {
    const refreshed = (event: Event) => {
      const { connectionKey: scope, batch } = (event as CustomEvent<{ connectionKey: string; batch: QuotaBatch }>).detail;
      if (scope !== connectionKey) return;
      const value = batch.items.find(item => item.account_id === account.id)?.usage;
      if (value) { ++request.current; lastRead.current = { identity, at: Date.now() }; acceptNative(value); }
    };
    document.addEventListener(usageRefreshEvent, refreshed);
    return () => document.removeEventListener(usageRefreshEvent, refreshed);
  }, [identity, connectionKey, account.id]);
  useEffect(() => {
    if (!online || !active || !account.usage?.native_read || running.current) return;
    if (lastRead.current.identity === identity && Date.now() - lastRead.current.at < 60_000) return;
    lastRead.current = { identity, at: Date.now() };
    const token = ++request.current;
    void api<UsageSummary>("GET", `/accounts/${account.id}/usage`).then(value => {
      if (request.current === token) acceptNative(value);
    }).catch(() => {
      if (request.current === token) acceptNative({ ...account.usage!, status: "error", message: "用量读取失败，请重新查询", windows: account.usage!.windows.map(w => w.source === "configured_quota" ? w : ({ ...w, status: "error" })) });
    });
  }, [identity, account.usage, account.id, online, active]);
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
      const token = ++request.current;
      const result = await accountOperation<{ message: string; account?: Account }>(account, action === "reset_quota" ? "reset_quota" : "usage", { action, confirmed }, account.operation_versions?.[action === "reset_quota" ? "reset_quota" : "usage"] ?? version);
      if (request.current !== token) return;
      if (result.account?.usage?.native_read) acceptNative(result.account.usage);
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
      {(usage?.branch === "apikey" || usage?.today) &&
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
                {w.label}{w.estimated && "（估算）"}
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
      {usage?.balances?.map((entry, i) => <span className="usage-money" key={`${entry.currency}:${i}`} title={`采集 ${fullTime(entry.observed_at)}`}>余额 {entry.currency || "币种未提供"} {entry.balance.toFixed(2)}{entry.status && entry.status !== "known" && ` · ${{ stale: "历史", error: "异常", unknown: "采集时间未知" }[entry.status]}`}</span>)}
      {usage?.message && <span className="usage-unknown" role={usage.status === "error" ? "status" : undefined}>{usage.message}</span>}
      {!windows.length && !usage?.message && !usage?.today && !usage?.balances?.length &&
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
      {account.recovery_monitor && !["cancelled", "recovered"].includes(account.recovery_monitor.status) && (!account.auto_reset_credit || !!account.auto_reset_credit.outcome) && (
        <details className="auto-reset-status">
          <summary>{account.recovery_monitor.label}</summary>
          {account.recovery_monitor.next_at && <time>下次 {fullTime(account.recovery_monitor.next_at)}</time>}
        </details>
      )}
      {account.auto_reset_credit && (!["manual", "recovered", "closed"].includes(account.auto_reset_credit.stage) || !!account.auto_reset_credit.outcome) && (
        <details className="auto-reset-status">
          <summary>{account.auto_reset_credit.outcome ? "最近用卡结果" : account.auto_reset_credit.label}</summary>
          {account.auto_reset_credit.outcome && <span>{account.auto_reset_credit.label}</span>}
          {!account.auto_reset_credit.outcome && (account.auto_reset_credit.verification_deadline_at || account.auto_reset_credit.consume_deadline_at) && <time>截止 {fullTime(account.auto_reset_credit.verification_deadline_at || account.auto_reset_credit.consume_deadline_at || "")}</time>}
          {account.auto_reset_credit.notification_status && <span>{{ pending: "结果通知待发送", delivered: "结果通知已发送", failed: "通知发送失败", suppressed: "Bark 已关闭" }[account.auto_reset_credit.notification_status] || ""}</span>}
          {account.auto_reset_credit.evidence_source && <span>依据：{account.auto_reset_credit.evidence_source === "account_rate_limit" ? "账号限流状态" : "上游 429 记录"}</span>}
          {account.auto_reset_credit.error && <span role="status">{account.auto_reset_credit.error}</span>}
          {account.auto_reset_credit.next_at && <time>下次 {fullTime(account.auto_reset_credit.next_at)}</time>}
          {account.auto_reset_credit.attempt_at && <time>用卡 {fullTime(account.auto_reset_credit.attempt_at)}</time>}
          {account.auto_reset_credit.test_completed_at && <time>测活 {fullTime(account.auto_reset_credit.test_completed_at)}</time>}
        </details>
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
