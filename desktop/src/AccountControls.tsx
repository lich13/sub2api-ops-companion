import { useEffect, useRef, useState } from "react";
import { Check, LoaderCircle, RefreshCw } from "lucide-react";
import { api, command } from "./bridge";
import {
  fullTime,
  type Account,
  type QuotaBatch,
  type Recovery,
} from "./types";
import { usageColor } from "./UsageCell";

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

export function PriorityEditor({
  account,
  online,
  report,
}: {
  account: Account;
  online: boolean;
  report: (e: unknown) => void;
}) {
  const [value, setValue] = useState(String(account.priority));
  const [busy, setBusy] = useState(false);
  const running = useRef(false);
  const version = useRef(account.version);
  useEffect(() => {
    setValue(String(account.priority));
    version.current = account.version;
  }, [account.priority, account.id]);
  async function save() {
    if (running.current || !online || value === String(account.priority))
      return;
    const priority = Number(value);
    if (
      !value ||
      !Number.isSafeInteger(priority) ||
      priority < 0 ||
      priority > 2147483647
    )
      return report("优先级必须是 0–2147483647 的整数");
    running.current = true;
    setBusy(true);
    try {
      await api("POST", `/accounts/${account.id}/priority`, {
        priority,
        expected_version: version.current,
      });
      await command("refresh");
    } catch (e) {
      report(e);
    } finally {
      running.current = false;
      setBusy(false);
    }
  }
  return (
    <div className="priority-edit">
      <input
        type="number"
        min="0"
        max="2147483647"
        value={value}
        aria-label={`${account.name}优先级`}
        disabled={!online || busy}
        onFocus={() => {
          version.current = account.version;
        }}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void save();
          if (e.key === "Escape") setValue(String(account.priority));
        }}
      />
      {value !== String(account.priority) && (
        <button
          title="保存优先级"
          disabled={busy || !online}
          onClick={() => void save()}
        >
          {busy ? (
            <LoaderCircle size={12} className="spin" />
          ) : (
            <Check size={12} />
          )}
        </button>
      )}
    </div>
  );
}

export function QuotaRefresh({
  online,
  report,
}: {
  online: boolean;
  report: (e: unknown) => void;
}) {
  const [batch, setBatch] = useState<QuotaBatch | null>(null);
  const [starting, setStarting] = useState(false);
  const running = useRef(false);
  useEffect(() => {
    if (!online) return;
    let gone = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const result = await api<QuotaBatch>("GET", "/quota-refresh");
        if (!gone) {
          setBatch(result);
          if (result.status === "running")
            timer = setTimeout(() => void poll(), 1000);
          else if (batch?.status === "running") void command("refresh");
        }
      } catch {
        if (!gone && batch?.status === "running")
          timer = setTimeout(() => void poll(), 1000);
      }
    }
    void poll();
    return () => {
      gone = true;
      clearTimeout(timer);
    };
  }, [online, batch?.id, batch?.status === "running"]);
  async function start() {
    if (running.current || !online) return;
    running.current = true;
    setStarting(true);
    try {
      setBatch(await api<QuotaBatch>("POST", "/quota-refresh", {}));
    } catch (e) {
      report(e);
    } finally {
      running.current = false;
      setStarting(false);
    }
  }
  const active = batch?.status === "running";
  const failures =
    batch?.items.filter(
      (i) => i.status === "failed" || i.status === "partial",
    ) ?? [];
  return (
    <div className="quota-batch">
      <button
        disabled={!online || starting || active}
        onClick={() => void start()}
      >
        <RefreshCw size={14} className={starting || active ? "spin" : ""} />
        刷新全部 OAuth 额度
      </button>
      {batch && batch.status !== "idle" && (
        <span>
          {batch.completed}/{batch.total}
          {active ? " 查询中" : " 已完成"}
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
  report,
}: {
  latest: Recovery[];
  accounts: Account[];
  online: boolean;
  report: (e: unknown) => void;
}) {
  const [rows, setRows] = useState<Recovery[]>([]);
  const [cursor, setCursor] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const running = useRef(false);
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
    if (!online) return;
    let gone = false;
    void api<{ items: Recovery[]; next_cursor: number | null }>(
      "GET",
      "/recoveries",
    )
      .then((r) => {
        if (!gone) {
          setRows(
            r.items.filter((row) => liveAccounts.current.has(row.account_id)),
          );
          setCursor(r.next_cursor);
        }
      })
      .catch(report);
    return () => {
      gone = true;
    };
  }, [online]);
  const live = new Set(accounts.map((a) => a.id));
  const items = [
    ...new Map(
      [...rows, ...latest]
        .filter((i) => live.has(i.account_id))
        .map((i) => [i.id, i]),
    ).values(),
  ].sort((a, b) => b.id - a.id);
  async function load(more = false) {
    if (running.current || !online) return;
    running.current = true;
    setBusy(true);
    try {
      const r = await api<{ items: Recovery[]; next_cursor: number | null }>(
        "GET",
        more ? `/recoveries?before_id=${cursor}` : "/recoveries",
      );
      setRows((old) =>
        (more ? [...old, ...r.items] : r.items).filter((row) =>
          liveAccounts.current.has(row.account_id),
        ),
      );
      setCursor(r.next_cursor);
    } catch (e) {
      report(e);
    } finally {
      running.current = false;
      setBusy(false);
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
              <th>测试模型</th>
              <th>测活通过时间</th>
              <th>恢复确认时间</th>
            </tr>
          </thead>
          <tbody>
            {items.map((r) => (
              <tr key={r.id}>
                <td>
                  {r.account_name ||
                    accounts.find((a) => a.id === r.account_id)?.name}
                </td>
                <td>{r.model_id || "未知"}</td>
                <td>
                  <time>
                    {r.test_completed_at
                      ? fullTime(r.test_completed_at)
                      : "时间未知"}
                  </time>
                </td>
                <td>
                  <time>
                    {r.recovered_at ? fullTime(r.recovered_at) : "时间未知"}
                  </time>
                </td>
              </tr>
            ))}
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
