import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Trash2 } from "lucide-react";
import { api, command } from "./bridge";
import type { Account } from "./types";

export type DeleteResult = {
  id: number;
  status: "waiting" | "running" | "deleted" | "failed";
  message?: string;
};

export function DeleteAccountsDialog({
  accounts,
  online,
  removed,
  finished,
  close,
}: {
  accounts: Account[];
  online: boolean;
  removed: (id: number) => void;
  finished: (results: DeleteResult[]) => void;
  close: () => void;
}) {
  const [results, setResults] = useState<DeleteResult[]>(
    accounts.map((a) => ({ id: a.id, status: "waiting" })),
  );
  const [phase, setPhase] = useState<"confirm" | "running" | "done">("confirm");
  const running = useRef(false),
    alive = useRef(true),
    connected = useRef(online);
  connected.current = online;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopImmediatePropagation();
        if (!running.current) close();
      }
    };
    window.addEventListener("keydown", escape, true);
    return () => window.removeEventListener("keydown", escape, true);
  }, [close]);
  async function start() {
    if (running.current || phase !== "confirm" || !connected.current) return;
    running.current = true;
    setPhase("running");
    const outcomes = accounts.map(
      (a): DeleteResult => ({ id: a.id, status: "waiting" }),
    );
    let next = 0;
    const update = (index: number, patch: Partial<DeleteResult>) => {
      outcomes[index] = { ...outcomes[index], ...patch };
      if (alive.current) setResults([...outcomes]);
    };
    async function worker() {
      while (next < accounts.length && alive.current) {
        const index = next++,
          account = accounts[index];
        if (!connected.current) {
          update(index, { status: "failed", message: "连接中断，未执行" });
          continue;
        }
        update(index, { status: "running" });
        try {
          const result = await api<{
            deleted: boolean;
            verified: boolean;
            detached: boolean;
          }>("DELETE", `/accounts/${account.id}`, {
            expected_version: account.version,
            detach_managed: account.managed,
          });
          if (!result.deleted || !result.verified)
            throw new Error("删除未确认，请刷新核对实际状态");
          update(index, {
            status: "deleted",
            message: result.detached ? "已解除托管并删除" : "已删除",
          });
          if (alive.current) removed(account.id);
        } catch (error) {
          update(index, {
            status: "failed",
            message: String(error).replace(/^Error: /, ""),
          });
        }
      }
    }
    await Promise.all(
      Array.from({ length: Math.min(3, accounts.length) }, worker),
    );
    running.current = false;
    if (alive.current) {
      setPhase("done");
      finished(outcomes);
      void command("refresh");
    }
  }
  return (
    <div className="modal-backdrop">
      <section
        className="modal delete-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="delete-title"
      >
        <Trash2 size={24} />
        <h2 id="delete-title">
          {phase === "confirm"
            ? `删除 ${accounts.length} 个账号？`
            : phase === "running"
              ? "正在删除账号"
              : "删除结果"}
        </h2>
        {phase === "confirm" && (
          <p>
            删除后无法撤销。
            {accounts.some((a) => a.managed) && "回退托管账号将先解除托管。"}
          </p>
        )}
        <ul className="delete-list">
          {accounts.map((a, i) => (
            <li key={a.id}>
              <div>
                <strong title={a.name}>{a.name}</strong>
                <span>
                  #{a.id}
                  {a.managed ? " · 回退托管" : ""}
                </span>
              </div>
              {phase !== "confirm" && (
                <span
                  className={
                    results[i].status === "failed"
                      ? "bad-text"
                      : results[i].status === "deleted"
                        ? "good-text"
                        : "muted"
                  }
                >
                  {results[i].status === "running" ? (
                    <>
                      <LoaderCircle size={12} className="spin" />
                      删除中
                    </>
                  ) : (
                    results[i].message || "等待中"
                  )}
                </span>
              )}
            </li>
          ))}
        </ul>
        {phase === "done" && (
          <p>
            {results.filter((r) => r.status === "deleted").length} 个已删除，
            {results.filter((r) => r.status === "failed").length} 个失败
          </p>
        )}
        <footer>
          <button disabled={phase === "running"} onClick={close}>
            {phase === "done" ? "关闭" : "取消"}
          </button>
          {phase !== "done" && (
            <button
              className="danger-button"
              disabled={!online || phase === "running"}
              onClick={() => void start()}
            >
              {phase === "running" ? "删除中" : "确认删除"}
            </button>
          )}
        </footer>
      </section>
    </div>
  );
}

export function RecoverStateButton({
  account,
  online,
  report,
}: {
  account: Account;
  online: boolean;
  report: (message: unknown) => void;
}) {
  const [busy, setBusy] = useState(false);
  const running = useRef(false);
  if (!account.recoverable) return null;
  async function recover() {
    if (running.current || !online) return;
    running.current = true;
    setBusy(true);
    try {
      const result = await api<{ verified: boolean }>(
        "POST",
        `/accounts/${account.id}/recover-state`,
        { expected_version: account.version },
      );
      report(
        result.verified
          ? `${account.name}：状态已恢复`
          : "恢复状态未确认，请刷新",
      );
    } catch (error) {
      report(error);
    } finally {
      running.current = false;
      setBusy(false);
      void command("refresh");
    }
  }
  return (
    <button disabled={!online || busy} onClick={() => void recover()}>
      {busy && <LoaderCircle size={12} className="spin" />}恢复状态
    </button>
  );
}
