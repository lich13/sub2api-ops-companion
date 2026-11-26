import { AccountOperationStatus } from "./OperationPanel";
import {
  memo,
  useCallback,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { MoreHorizontal, X } from "lucide-react";
import { command } from "./bridge";
import { fullTime, type Account } from "./types";
import SchedulingParameters from "./SchedulingParameters";
import {
  AccountActionButton,
  isPrimaryAction,
  type AccountActionProvider,
} from "./AccountActions";
import UsageCell from "./UsageCell";
import {
  isCleanEscape,
  useBackdropDismiss,
  useDialogFocus,
} from "./dialogFocus";
import { useBackAction } from "./mobile";
import { supportsModelDetection } from "./modelTestTypes";
import { DegradationBadge } from "./DegradationMark";
import AccountStatus from "./AccountStatus";

type Props = {
  accounts: Account[];
  online: boolean;
  selected: Set<number>;
  busyAccountId?: number | null;
  active?: boolean;
  connectionKey?: string;
  select: (id: number, checked: boolean) => void;
  schedule: (
    account: Account,
    disabled?: boolean,
    isBusy?: boolean,
  ) => ReactNode;
  error: (id: number) => void;
  report: (error: unknown) => void;
  accountActions: AccountActionProvider;
};

const refreshSnapshot = () => command("refresh");

const MobileAccountCard = memo(function MobileAccountCard({
  account: a,
  online,
  isSelected,
  isSchedulingBusy,
  actionState: _actionState,
  active,
  connectionKey,
  select,
  schedule,
  error,
  report,
  accountActions,
  openActions,
}: {
  account: Account;
  online: boolean;
  isSelected: boolean;
  isSchedulingBusy: boolean;
  actionState: string;
  active?: boolean;
  connectionKey?: string;
  select: (id: number, checked: boolean) => void;
  schedule: (
    account: Account,
    disabled?: boolean,
    isBusy?: boolean,
  ) => ReactNode;
  error: (id: number) => void;
  report: (error: unknown) => void;
  accountActions: AccountActionProvider;
  openActions: (id: number) => void;
}) {
  return (
    <article className="mobile-account">
      <header>
        <label className="account-check">
          <input
            type="checkbox"
            aria-label={`选择 ${a.name}`}
            checked={isSelected}
            disabled={!online}
            onChange={(e) => select(a.id, e.target.checked)}
          />
        </label>
        <div className="mobile-identity">
          <strong title={a.name}>{a.name}</strong>
          <span>
            {a.platform === "openai"
              ? "OpenAI"
              : a.platform === "grok"
                ? "Grok"
                : a.platform}{" "}
            ·{" "}
            {a.type === "oauth"
              ? "OAuth"
              : a.type === "apikey"
                ? "Key"
                : a.type}
          </span>
        </div>
        {schedule(a, false, isSchedulingBusy)}
        <button
          className="icon-button"
          aria-label={`${a.name}操作`}
          onClick={() => openActions(a.id)}
        >
          <MoreHorizontal size={21} />
        </button>
      </header>
      <div className="mobile-account-status">
        <DegradationBadge account={a} />
        <AccountOperationStatus id={a.id} />
        <AccountStatus account={a} onError={error} />
      </div>
      <UsageCell
        account={a}
        online={online}
        active={active}
        connectionKey={connectionKey}
        refresh={refreshSnapshot}
        report={report}
      />
      <footer>
        <div className="mobile-error-summary">
          {a.last_error_id ? (
            <button
              className="mobile-error"
              onClick={() => error(a.last_error_id!)}
            >
              <span className="bad-text">
                {a.last_error_code || a.last_error_status || "上游错误"}
              </span>
              <time>{fullTime(a.last_error_at)}</time>
              {a.success_after_error && (
                <span className="good-text">之后已有成功调用</span>
              )}
            </button>
          ) : (
            <span className="muted">
              {a.error_message ? "错误时间未知" : "暂无错误记录"}
            </span>
          )}
          <div className="account-last-success">
            <span>最后成功</span>
            <time className={a.success_after_error ? "good-text" : undefined}>
              {fullTime(a.last_success_at)}
            </time>
          </div>
        </div>
        <span className="priority-value">优先级 {a.priority}</span>
      </footer>
      {supportsModelDetection(a) && (
        <div className="mobile-primary-actions">
          {accountActions(a)
            .filter(isPrimaryAction)
            .map((item) => (
              <AccountActionButton key={item.id} item={item} />
            ))}
        </div>
      )}
    </article>
  );
});

export default function MobileAccounts(props: Props) {
  const [activeId, setActiveId] = useState<number | null>(null);
  const accountById = useMemo(
    () => new Map(props.accounts.map((a) => [a.id, a])),
    [props.accounts],
  );
  const active = activeId != null ? accountById.get(activeId) : undefined;
  const sheetRef = useRef<HTMLElement>(null);
  useDialogFocus(sheetRef, !!active);
  const closeSheet = useCallback(() => setActiveId(null), []);
  const sheetBackdrop = useBackdropDismiss(closeSheet);
  useBackAction(!!active, closeSheet);
  const openActions = useCallback((id: number) => setActiveId(id), []);
  return (
    <>
      <div className="mobile-accounts">
        {props.accounts.map((a) => (
          <MobileAccountCard
            key={a.id}
            account={a}
            online={props.online}
            isSelected={props.selected.has(a.id)}
            isSchedulingBusy={props.busyAccountId === a.id}
            actionState={props.accountActions.getAccountState?.(a.id) ?? ""}
            active={props.active}
            connectionKey={props.connectionKey}
            select={props.select}
            schedule={props.schedule}
            error={props.error}
            report={props.report}
            accountActions={props.accountActions}
            openActions={openActions}
          />
        ))}
      </div>
      {active && (
        <div className="modal-backdrop" {...sheetBackdrop}>
          <section
            ref={sheetRef}
            className="mobile-action-sheet"
            role="dialog"
            aria-modal="true"
            aria-label="账号操作"
            tabIndex={-1}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (isCleanEscape(e)) {
                e.preventDefault();
                e.stopPropagation();
                closeSheet();
              }
            }}
          >
            <header>
              <h2>{active.name}</h2>
              <button
                className="icon-button"
                aria-label="关闭账号操作"
                onClick={() => setActiveId(null)}
              >
                <X size={20} />
              </button>
            </header>
            <SchedulingParameters account={active} online={props.online} />
            <div className="mobile-more-actions">
              {props
                .accountActions(active)
                .filter((item) => !isPrimaryAction(item))
                .map((item) => (
                  <AccountActionButton
                    key={item.id}
                    item={item}
                    after={() => setActiveId(null)}
                  />
                ))}
            </div>
          </section>
        </div>
      )}
    </>
  );
}
