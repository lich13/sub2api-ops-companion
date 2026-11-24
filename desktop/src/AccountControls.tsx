import { useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { api, command } from "./bridge";
import { fullTime, type Account, type QuotaBatch } from "./types";
import { usageColor, usageRefreshEvent } from "./UsageCell";

export function MiniUsage({ account }: { account: Account }) {
  if (account.type !== "oauth") return null;
  const windows = (account.usage?.windows ?? account.usage_windows).filter(
    (w) => ["5h", "7d", "30d", "24h"].includes(w.label),
  );
  if (!windows.length) return null;
  return (
    <div className="mini-usage" aria-label={`${account.name}用量`}>
      {windows.map((w) => (
        <span
          key={w.key}
          title={`采集 ${fullTime(w.observed_at)} · 重置 ${fullTime(w.reset_at)}${w.status === "stale" ? " · 历史快照" : w.status === "error" ? " · 查询异常" : ""}`}
        >
          <b>{w.label}</b>
          <i
            className={`mini-track ${w.used_percent == null ? "" : usageColor(w.used_percent)}`}
          >
            {w.used_percent != null && (
              <i
                style={{
                  width: `${Math.max(0, Math.min(100, w.used_percent))}%`,
                }}
              />
            )}
          </i>
          <em>
            {w.used_percent == null ? "未知" : `${Math.round(w.used_percent)}%`}
            {w.status === "stale" ? "*" : w.status === "error" ? "!" : ""}
          </em>
        </span>
      ))}
    </div>
  );
}

export function QuotaRefresh({
  online,
  report,
  active = true,
  connectionKey = "",
}: {
  online: boolean;
  report: (e: unknown) => void;
  active?: boolean;
  connectionKey?: string;
}) {
  const [batch, setBatch] = useState<QuotaBatch | null>(null);
  const [starting, setStarting] = useState(false);
  const [pollRun, setPollRun] = useState(0);
  const running = useRef(false);
  const wasRunning = useRef(false);
  const skipInitialFetch = useRef(false);
  useEffect(() => {
    wasRunning.current = false;
    skipInitialFetch.current = false;
  }, [connectionKey]);
  useEffect(() => {
    if (!online || !active) return;
    let gone = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const result = await api<QuotaBatch>("GET", "/quota-refresh");
        if (!gone) {
          setBatch(result);
          if (result.status === "running") {
            wasRunning.current = true;
            timer = setTimeout(() => void poll(), 1000);
          } else if (wasRunning.current) {
            wasRunning.current = false;
            document.dispatchEvent(
              new CustomEvent(usageRefreshEvent, {
                detail: { connectionKey, batch: result },
              }),
            );
            void command("refresh");
          }
        }
      } catch {
        if (!gone && wasRunning.current)
          timer = setTimeout(() => void poll(), 1000);
      }
    }
    if (skipInitialFetch.current) {
      skipInitialFetch.current = false;
      if (wasRunning.current) timer = setTimeout(() => void poll(), 1000);
    } else {
      void poll();
    }
    return () => {
      gone = true;
      clearTimeout(timer);
    };
  }, [online, active, connectionKey, pollRun]);
  async function start() {
    if (running.current || !online) return;
    running.current = true;
    setStarting(true);
    try {
      const result = await api<QuotaBatch>("POST", "/quota-refresh", {});
      setBatch(result);
      if (result.status === "running") {
        wasRunning.current = true;
        skipInitialFetch.current = true;
        setPollRun((n) => n + 1);
      } else {
        wasRunning.current = false;
        if (result.status !== "idle") {
          document.dispatchEvent(
            new CustomEvent(usageRefreshEvent, {
              detail: { connectionKey, batch: result },
            }),
          );
          void command("refresh");
        }
      }
    } catch (e) {
      report(e);
    } finally {
      running.current = false;
      setStarting(false);
    }
  }
  const inProgress = batch?.status === "running";
  const failures =
    batch?.items.filter(
      (i) => i.status === "failed" || i.status === "partial",
    ) ?? [];
  return (
    <div className="quota-batch">
      <button
        disabled={!online || starting || inProgress}
        onClick={() => void start()}
      >
        <RefreshCw size={14} className={starting || inProgress ? "spin" : ""} />
        刷新全部用量
      </button>
      {batch && batch.status !== "idle" && (
        <span>
          {batch.completed}/{batch.total}
          {inProgress ? " 查询中" : " 已完成"}
        </span>
      )}
      {!!failures.length && (
        <details>
          <summary>{failures.length} 个账号未完整更新</summary>
          <ul>
            {failures.map((i) => (
              <li key={i.account_id}>
                {i.account_name}：{i.error || "部分窗口查询失败"}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
