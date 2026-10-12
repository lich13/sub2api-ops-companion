import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Trash2 } from "lucide-react";
import { command } from "./bridge";
import { accountOperation, followAccountBatch, operationEpoch, operationTerminal, submitAccountBatch } from "./accountOperations";
import type { Account } from "./types";
import { useBackAction } from "./mobile";

export type DeleteResult = {
  id: number;
  status: "waiting" | "running" | "deleted" | "failed";
  message?: string;
};

export function DeleteAccountsDialog({
  accounts,
  allAccounts = accounts,
  online,
  removed,
  finished,
  close,
}: {
  accounts: Account[];
  allAccounts?: Account[];
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
  const requestId = useRef(crypto.randomUUID()), accepted = useRef(false);
  const [error, setError] = useState("");
  const affected = allAccounts.filter(a => a.parent_account_id && accounts.some(parent => parent.id === a.parent_account_id));
  connected.current = online;
  useBackAction(true, close);
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
        close();
      }
    };
    window.addEventListener("keydown", escape, true);
    return () => window.removeEventListener("keydown", escape, true);
  }, [close]);
  async function start() {
    if (running.current || phase !== "confirm" || !connected.current) return;
    running.current = true;
    const epoch = operationEpoch();
    setPhase("running");
    setError("");
    try {
      const batch = await submitAccountBatch("delete", accounts.map(a => {
        if (!a.operation_versions?.delete) throw new Error("请刷新账号后重试");
        return { account_id: a.id, expected_version: a.operation_versions.delete, payload: { detach_managed: a.managed } };
      }), requestId.current);
      accepted.current = true;
      await followAccountBatch(batch, value => {
        const outcomes = accounts.map((a): DeleteResult => {
          const job = value.items.find(j => j.account_id === a.id);
          const deleted = job?.status === 'completed' && job.result?.deleted === true;
          if (deleted) { removed(a.id); affected.filter(child => child.parent_account_id === a.id).forEach(child => removed(child.id)); }
          return { id: a.id, status: deleted ? 'deleted' : job && operationTerminal.has(job.status) ? 'failed' : job?.status === 'queued' ? 'waiting' : 'running',
            message: deleted ? '已删除' : job?.reason || (job?.status === 'queued' ? '已排队' : job?.status === 'checking' ? '正在核对' : '删除中') };
        });
        setResults(outcomes);
        if (value.items.every(j => operationTerminal.has(j.status))) { setPhase('done'); finished(outcomes); }
      }, () => alive.current && operationEpoch() === epoch);
    } catch (cause) {
      if (alive.current && operationEpoch() === epoch) {
        setError(`${String(cause).replace(/^Error: /, '')}。${accepted.current ? '任务已保留，可在账号待办查看。' : '重试将复用本次请求。'}`);
        setPhase('confirm');
      }
    } finally {
      running.current = false;
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
            {[...accounts, ...affected].some((a) => a.managed) && "回退托管账号将先解除托管。"}
            {!!affected.length && `母账号关联的 ${affected.length} 个账号也会被删除。`}
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
                      {results[i].message || '删除中'}
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
        {error && <p className="bad-text" role="alert">{error}</p>}
        <footer>
          <button onClick={close}>
            {phase === "confirm" ? "取消" : "关闭"}
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
      const result = await accountOperation<{ verified: boolean }>(
        account, "recover", {},
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
