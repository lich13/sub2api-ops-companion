import { useCallback, useEffect, useRef, useState } from "react";
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
  const [batchState, setBatchState] = useState<{
    key: string;
    batch: QuotaBatch | null;
  }>({ key: connectionKey, batch: null });
  const [startingState, setStartingState] = useState<{
    key: string;
    epoch: number;
  } | null>(null);
  const prevKeyRef = useRef(connectionKey);
  const sessionEpochRef = useRef(0);
  const settledPostSeqRef = useRef(0);
  const startingFlightRef = useRef<number | null>(null);
  const runningKeyRef = useRef<string | null>(null);
  const activeSessionRef = useRef<string | null>(null);
  const latestKeyRef = useRef(connectionKey);
  const onlineActiveRef = useRef(online && active);
  const batch = batchState.key === connectionKey ? batchState.batch : null;
  const currentEpoch =
    prevKeyRef.current === connectionKey
      ? sessionEpochRef.current
      : sessionEpochRef.current + 1;
  const starting =
    startingState !== null &&
    startingState.key === connectionKey &&
    startingState.epoch === currentEpoch;
  useEffect(() => {
    latestKeyRef.current = connectionKey;
    onlineActiveRef.current = online && active;
    if (prevKeyRef.current !== connectionKey) {
      prevKeyRef.current = connectionKey;
      sessionEpochRef.current += 1;
      runningKeyRef.current = null;
    } else if (
      runningKeyRef.current !== null &&
      runningKeyRef.current !== connectionKey
    ) {
      runningKeyRef.current = null;
    }
  }, [connectionKey, online, active]);
  const notifyRefreshFinished = useCallback(
    (targetKey: string, result: QuotaBatch) => {
      document.dispatchEvent(
        new CustomEvent(usageRefreshEvent, {
          detail: { connectionKey: targetKey, batch: result },
        }),
      );
      void command("refresh");
    },
    [],
  );
  const applyBatchResult = useCallback(
    (targetKey: string, result: QuotaBatch, fromManualStart = false) => {
      if (targetKey !== latestKeyRef.current) return;
      setBatchState({ key: targetKey, batch: result });
      if (result.status === "running") {
        runningKeyRef.current = targetKey;
      } else {
        const wasRunning = runningKeyRef.current === targetKey;
        if (wasRunning) {
          runningKeyRef.current = null;
        }
        if ((wasRunning || fromManualStart) && result.status !== "idle") {
          notifyRefreshFinished(targetKey, result);
        }
      }
    },
    [notifyRefreshFinished],
  );
  const inProgress = batch?.status === "running";
  useEffect(() => {
    if (!online || !active) {
      activeSessionRef.current = null;
      return;
    }
    const effectEpoch = sessionEpochRef.current;
    const resumedOrInitial = activeSessionRef.current !== connectionKey;
    if (!inProgress && !resumedOrInitial) return;
    let gone = false;
    let failures = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const scheduleNext = (delayMs: number) => {
      timer = setTimeout(() => void poll(), delayMs);
    };
    async function poll() {
      const seenPostSeq = settledPostSeqRef.current;
      try {
        const result = await api<QuotaBatch>("GET", "/quota-refresh");
        if (
          gone ||
          effectEpoch !== sessionEpochRef.current ||
          seenPostSeq !== settledPostSeqRef.current
        ) {
          return;
        }
        activeSessionRef.current = connectionKey;
        failures = 0;
        applyBatchResult(connectionKey, result);
        if (result.status === "running") {
          scheduleNext(1000);
        }
      } catch {
        if (
          gone ||
          effectEpoch !== sessionEpochRef.current ||
          seenPostSeq !== settledPostSeqRef.current
        ) {
          return;
        }
        activeSessionRef.current = connectionKey;
        if (runningKeyRef.current === connectionKey) {
          const delay = Math.min(15_000, 1000 * 2 ** failures);
          failures = Math.min(failures + 1, 4);
          scheduleNext(delay);
        }
      }
    }
    if (inProgress) {
      runningKeyRef.current = connectionKey;
      if (resumedOrInitial) void poll();
      else scheduleNext(1000);
    } else {
      void poll();
    }
    return () => {
      gone = true;
      clearTimeout(timer);
    };
  }, [online, active, connectionKey, inProgress, applyBatchResult]);
  async function start() {
    const startEpoch =
      prevKeyRef.current === connectionKey
        ? sessionEpochRef.current
        : sessionEpochRef.current + 1;
    if (startingFlightRef.current === startEpoch || !online) return;
    const targetKey = connectionKey;
    startingFlightRef.current = startEpoch;
    setStartingState({ key: targetKey, epoch: startEpoch });
    try {
      const result = await api<QuotaBatch>("POST", "/quota-refresh", {});
      if (
        targetKey !== latestKeyRef.current ||
        startEpoch !== sessionEpochRef.current
      ) {
        return;
      }
      settledPostSeqRef.current += 1;
      if (onlineActiveRef.current) {
        activeSessionRef.current = targetKey;
      }
      applyBatchResult(targetKey, result, true);
    } catch (e) {
      if (
        targetKey === latestKeyRef.current &&
        startEpoch === sessionEpochRef.current
      ) {
        report(e);
      }
    } finally {
      if (startingFlightRef.current === startEpoch) {
        startingFlightRef.current = null;
      }
      setStartingState((prev) => (prev?.epoch === startEpoch ? null : prev));
    }
  }
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
