import { useEffect, useMemo, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { api, command } from "./bridge";
import {
  fullTime,
  type Account,
  type QuotaBatch,
  type Recovery,
} from "./types";
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
            document.dispatchEvent(new CustomEvent(usageRefreshEvent, { detail: { connectionKey, batch: result } }));
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
          document.dispatchEvent(new CustomEvent(usageRefreshEvent, { detail: { connectionKey, batch: result } }));
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

export function RecoveryHistory({
  latest,
  accounts,
  online,
  active = true,
  onCount,
  report,
}: {
  latest: Recovery[];
  accounts: Account[];
  online: boolean;
  active?: boolean;
  onCount?: (count: number) => void;
  report: (e: unknown) => void;
}) {
  const [rows, setRows] = useState<Recovery[]>([]);
  const [cursor, setCursor] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const running = useRef(false);
  const generation = useRef(0);
  const enabled = useRef(active && online);
  enabled.current = active && online;
  const liveAccounts = useRef(new Set<number>());
  liveAccounts.current = new Set(accounts.map((a) => a.id));
  useEffect(() => {
    setRows((old) =>
      old.every((row) => liveAccounts.current.has(row.account_id))
        ? old
        : old.filter((row) => liveAccounts.current.has(row.account_id)),
    );
  }, [accounts]);
  useEffect(() => {
    if (active && online) void load();
    return () => { ++generation.current; running.current = false; };
  }, [online, active]);
  const accountNameById = useMemo(
    () => new Map(accounts.map((a) => [a.id, a.name])),
    [accounts],
  );
  const items = useMemo(
    () =>
      [
        ...new Map(
          [...rows, ...latest]
            .filter((i) => accountNameById.has(i.account_id))
            .map((i) => [i.id, i]),
        ).values(),
      ].sort((a, b) => b.id - a.id),
    [rows, latest, accountNameById],
  );
  useEffect(() => { onCount?.(items.length); }, [items.length, onCount]);
  async function load(more = false) {
    if (running.current || !online || !active) return;
    const epoch = ++generation.current;
    const valid = () => epoch === generation.current && enabled.current;
    running.current = true;
    setBusy(true);
    try {
      const r = await api<{ items: Recovery[]; next_cursor: number | null }>(
        "GET",
        more ? `/recoveries?before_id=${cursor}` : "/recoveries",
      );
      if (!valid()) return;
      setRows((old) =>
        (more ? [...old, ...r.items] : r.items).filter((row) =>
          liveAccounts.current.has(row.account_id),
        ),
      );
      setCursor(r.next_cursor);
    } catch (e) {
      if (valid()) report(e);
    } finally {
      if (valid()) { running.current = false; setBusy(false); }
    }
  }
  return (
    <>
      <div className="section-heading">
        <span>{items.length} 条记录</span>
        <button
          className="text-button"
          disabled={!online || busy}
          onClick={() => void load()}
        >
          刷新记录
        </button>
      </div>
      <div className="table-wrap recovery-table">
        <table>
          <thead>
            <tr>
              <th>账号</th>
              <th>恢复类型</th>
              <th>验证方式</th>
              <th>测试模型</th>
              <th className="recovery-attempts">尝试次数</th>
              <th>用卡时间</th>
              <th>测试完成时间</th>
              <th>结果 / 恢复时间</th>
            </tr>
          </thead>
          <tbody>
            {items.map((r) => {
              const scheduled = r.kind === "connection_test";
              const card = r.kind === "reset_credit" && !r.legacy && r.reset_credit
                && ["connection", "model"].includes(r.reset_credit.verification_method)
                && [r.reset_credit.completed_at, r.test_completed_at, r.recovered_at].every((time) => !!time && Number.isFinite(Date.parse(time)))
                ? r.reset_credit : undefined;
              return (
              <tr key={r.id}>
                <td data-label="账号">
                  {r.account_name || accountNameById.get(r.account_id)}
                </td>
                <td data-label="恢复类型">{scheduled ? "预约测试" : card ? "用卡恢复" : "额度恢复"}</td>
                <td data-label="验证方式">{scheduled ? "测试连接" : card ? card.verification_method === "model" ? "模型测试" : "测试连接" : "—"}</td>
                <td data-label="测试模型">{r.model_id || "未知"}</td>
                <td data-label="尝试次数" className="recovery-attempts">{r.attempt_count ?? "—"}</td>
                <td data-label="用卡时间"><time>{card ? fullTime(card.completed_at) : "—"}</time></td>
                <td data-label="测试完成时间">
                  <time>
                    {r.test_completed_at
                      ? fullTime(r.test_completed_at)
                      : "时间未知"}
                  </time>
                </td>
                <td data-label="结果 / 恢复时间">
                  <time>
                    {scheduled ? ({completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needs_confirmation: "需确认"}[r.result_status ?? ""] ?? "已记录") : r.recovered_at ? fullTime(r.recovered_at) : "时间未知"}
                  </time>
                </td>
              </tr>
            ); })}
          </tbody>
        </table>
        {!items.length && <div className="quiet">暂无恢复记录</div>}
      </div>
      {cursor != null && (
        <button
          className="load-more"
          disabled={busy || !online}
          onClick={() => void load(true)}
        >
          加载更早恢复记录
        </button>
      )}
    </>
  );
}
