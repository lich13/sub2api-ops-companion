import { useEffect, useRef, useState } from "react";
import { LoaderCircle, RotateCcw } from "lucide-react";
import {
  followAccountBatch,
  operationEpoch,
  operationTerminal,
  submitAccountBatch,
} from "./accountOperations";
import type { Account } from "./types";

export const resetCreditEligible = (account: Account) =>
  !!account.usage?.actions.includes("reset_quota") &&
  (account.usage.reset_credits?.available ?? 0) > 0;

export function RecoverSelectedButton({
  accounts,
  online,
  report,
}: {
  accounts: Account[];
  online: boolean;
  report: (message: unknown) => void;
}) {
  const [busy, setBusy] = useState(false);
  const running = useRef(false),
    alive = useRef(true);
  const request = useRef<{ signature: string; id: string } | null>(null);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const eligible = accounts.filter((account) => account.recoverable);
  async function recover() {
    if (running.current || !online || !eligible.length) return;
    running.current = true;
    setBusy(true);
    const epoch = operationEpoch(),
      skipped = accounts.length - eligible.length;
    const current = () => alive.current && operationEpoch() === epoch;
    try {
      const targets = eligible
        .map((account) => ({
          account_id: account.id,
          expected_version: account.operation_versions.recover,
        }))
        .sort((a, b) => a.account_id - b.account_id);
      const signature = JSON.stringify(targets);
      if (request.current?.signature !== signature)
        request.current = { signature, id: crypto.randomUUID() };
      const batch = await submitAccountBatch(
        "recover",
        targets,
        request.current.id,
      );
      await followAccountBatch(
        batch,
        (value) => {
          if (!value.items.every((item) => operationTerminal.has(item.status)))
            return;
          const restored = value.items.filter(
            (item) =>
              item.status === "completed" &&
              item.result?.verified === true &&
              !item.result.unchanged,
          ).length;
          const unchanged =
            skipped +
            value.items.filter(
              (item) =>
                item.status === "completed" &&
                item.result?.verified === true &&
                item.result.unchanged,
            ).length;
          const uncertain = value.items.filter(
            (item) => item.status === "needs_confirmation",
          ).length;
          const failed =
            value.items.length + skipped - restored - unchanged - uncertain;
          report(
            `已恢复 ${restored} 个 · 无需恢复 ${unchanged} 个 · 失败 ${failed} 个 · 待核对 ${uncertain} 个`,
          );
        },
        current,
      );
    } catch (error) {
      if (current()) report(error);
    } finally {
      running.current = false;
      if (current()) setBusy(false);
    }
  }
  return (
    <button
      disabled={!online || busy || !eligible.length}
      onClick={() => void recover()}
    >
      {busy ? (
        <LoaderCircle size={12} className="spin" />
      ) : (
        <RotateCcw size={12} />
      )}
      恢复状态
    </button>
  );
}
