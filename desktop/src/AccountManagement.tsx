import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Ticket, Trash2 } from "lucide-react";
import { command } from "./bridge";
import { errorMessage } from "./bridgeError";
import {
  accountOperation,
  followAccountBatch,
  operationEpoch,
  operationTerminal,
  submitAccountBatch,
} from "./accountOperations";
import type { Account } from "./types";
import { useBackAction } from "./mobile";
import {
  RecoverSelectedButton,
  resetCreditEligible,
} from "./AccountRecoveryButton";

export { RecoverSelectedButton, resetCreditEligible };

type DeleteResult = {
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
  const requestId = useRef(crypto.randomUUID()),
    accepted = useRef(false);
  const [error, setError] = useState("");
  const affected = allAccounts.filter(
    (a) =>
      a.parent_account_id &&
      accounts.some((parent) => parent.id === a.parent_account_id),
  );
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
      if (event.key === "Escape" && !event.isComposing) {
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
      const batch = await submitAccountBatch(
        "delete",
        accounts.map((a) => ({
          account_id: a.id,
          expected_version: a.operation_versions.delete,
          payload: {},
        })),
        requestId.current,
      );
      accepted.current = true;
      await followAccountBatch(
        batch,
        (value) => {
          const outcomes = accounts.map((a): DeleteResult => {
            const job = value.items.find((j) => j.account_id === a.id);
            const deleted =
              job?.status === "completed" && job.result?.deleted === true;
            if (deleted) {
              removed(a.id);
              affected
                .filter((child) => child.parent_account_id === a.id)
                .forEach((child) => removed(child.id));
            }
            return {
              id: a.id,
              status: deleted
                ? "deleted"
                : job && operationTerminal.has(job.status)
                  ? "failed"
                  : job?.status === "queued"
                    ? "waiting"
                    : "running",
              message: deleted
                ? "已删除"
                : job?.reason ||
                  (job?.status === "queued"
                    ? "已排队"
                    : job?.status === "checking"
                      ? "正在核对"
                      : "删除中"),
            };
          });
          setResults(outcomes);
          if (value.items.every((j) => operationTerminal.has(j.status))) {
            setPhase("done");
            finished(outcomes);
          }
        },
        () => alive.current && operationEpoch() === epoch,
      );
    } catch (cause) {
      if (alive.current && operationEpoch() === epoch) {
        setError(
          `${errorMessage(cause)}。${accepted.current ? "任务已保留，可在账号待办查看。" : "重试将复用本次请求。"}`,
        );
        setPhase("confirm");
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
            {!!affected.length &&
              `母账号关联的 ${affected.length} 个账号也会被删除。`}
          </p>
        )}
        <ul className="delete-list">
          {accounts.map((a, i) => (
            <li key={a.id}>
              <div>
                <strong title={a.name}>{a.name}</strong>
                <span>#{a.id}</span>
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
                      {results[i].message || "删除中"}
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
        {error && (
          <p className="bad-text" role="alert">
            {error}
          </p>
        )}
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

type ResetCreditResult = {
  status: "waiting" | "running" | "done" | "failed" | "uncertain";
  message?: string;
};

export function ResetCreditsDialog({
  accounts,
  online,
  report,
  close,
}: {
  accounts: Account[];
  online: boolean;
  report: (message: unknown) => void;
  close: () => void;
}) {
  // Targets and their versions are fixed when the dialog opens; the server refuses a card that changed since.
  const [targets] = useState(() => accounts.filter(resetCreditEligible));
  const skipped = accounts.length - targets.length;
  const [results, setResults] = useState<ResetCreditResult[]>(() =>
    targets.map(() => ({ status: "waiting" })),
  );
  const [phase, setPhase] = useState<"confirm" | "running" | "done">("confirm");
  const running = useRef(false),
    alive = useRef(true),
    connected = useRef(online);
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
      if (event.key === "Escape" && !event.isComposing) {
        event.stopImmediatePropagation();
        close();
      }
    };
    window.addEventListener("keydown", escape, true);
    return () => window.removeEventListener("keydown", escape, true);
  }, [close]);
  async function start() {
    if (
      running.current ||
      phase !== "confirm" ||
      !connected.current ||
      !targets.length
    )
      return;
    running.current = true;
    const epoch = operationEpoch();
    const current = () => alive.current && operationEpoch() === epoch;
    setPhase("running");
    let used = 0,
      failed = 0,
      uncertain = 0,
      attempted = 0;
    for (const [index, account] of targets.entries()) {
      // Closing the dialog, going offline or switching connections stops before the next card is spent.
      if (!current() || !connected.current) break;
      const show = (value: ResetCreditResult) => {
        if (current())
          setResults((old) =>
            old.map((item, i) => (i === index ? value : item)),
          );
      };
      ++attempted;
      show({ status: "running" });
      let status = "";
      try {
        const result = await accountOperation<{ message?: string }>(
          account,
          "reset_quota",
          { action: "reset_quota", confirmed: true },
          undefined,
          (job) => {
            status = job.status;
          },
        );
        ++used;
        show({ status: "done", message: result?.message || "重置已执行" });
      } catch (error) {
        const message = errorMessage(error);
        // An accepted request without a definite failure may still have spent the card.
        if (status && !["failed", "cancelled", "superseded"].includes(status)) {
          ++uncertain;
          show({ status: "uncertain", message: `待核对：${message}` });
        } else {
          ++failed;
          show({ status: "failed", message });
        }
      }
    }
    running.current = false;
    if (operationEpoch() !== epoch) return;
    void command("refresh");
    const pending = targets.length - attempted;
    report(
      `已使用 ${used} 张重置卡 · 失败 ${failed} 个 · 待核对 ${uncertain} 个 · 跳过 ${skipped} 个${pending ? ` · 未执行 ${pending} 个` : ""}`,
    );
    if (alive.current) setPhase("done");
  }
  const weekly = (account: Account) =>
    account.usage?.windows.find((window) => window.key === "codex_7d")
      ?.used_percent;
  const count = (status: ResetCreditResult["status"]) =>
    results.filter((result) => result.status === status).length;
  return (
    <div className="modal-backdrop">
      <section
        className="modal delete-dialog reset-credits-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="reset-credits-title"
      >
        <Ticket size={24} />
        <h2 id="reset-credits-title">
          {phase === "confirm"
            ? `对 ${targets.length} 个账号使用重置卡？`
            : phase === "running"
              ? "正在使用重置卡"
              : "用卡结果"}
        </h2>
        {phase === "confirm" && (
          <p>
            每个账号消耗一次重置次数并执行 Sub2API 的额度重置，无法撤销。
            {!!skipped && `已跳过 ${skipped} 个没有可用重置卡的账号。`}
          </p>
        )}
        {phase === "running" && <p>逐个执行中；关闭后不再继续后面的账号。</p>}
        <ul className="delete-list">
          {targets.map((account, index) => {
            const result = results[index],
              percent = weekly(account);
            return (
              <li key={account.id}>
                <div>
                  <strong title={account.name}>{account.name}</strong>
                  <span>
                    #{account.id} · 7d{" "}
                    {percent == null ? "未知" : `${Math.round(percent)}%`} ·
                    可用 {account.usage?.reset_credits?.available ?? 0} 张
                  </span>
                </div>
                {phase !== "confirm" && (
                  <span
                    className={
                      result.status === "failed" ||
                      result.status === "uncertain"
                        ? "bad-text"
                        : result.status === "done"
                          ? "good-text"
                          : "muted"
                    }
                  >
                    {result.status === "running" ? (
                      <>
                        <LoaderCircle size={12} className="spin" />
                        重置中
                      </>
                    ) : (
                      result.message || (phase === "done" ? "未执行" : "等待中")
                    )}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
        {phase === "done" && (
          <p>
            {count("done")} 个已重置，{count("failed")} 个失败，
            {count("uncertain")} 个待核对
          </p>
        )}
        <footer>
          <button onClick={close}>
            {phase === "confirm" ? "取消" : "关闭"}
          </button>
          {phase !== "done" && (
            <button
              className="primary"
              disabled={!online || phase === "running" || !targets.length}
              onClick={() => void start()}
            >
              {phase === "running" ? "使用中" : "确认使用"}
            </button>
          )}
        </footer>
      </section>
    </div>
  );
}
