import { AccountOperationStatus } from "./OperationPanel";
import { memo, useCallback, useState, type ReactNode } from "react";
import { MoreHorizontal, X } from "lucide-react";
import { command } from "./bridge";
import { fullTime, type Account } from "./types";
import SchedulingParameters from "./SchedulingParameters";
import {
  AccountActionButton,
  isPrimaryAction,
  type AccountActionItem,
} from "./AccountActions";
import UsageCell from "./UsageCell";
import { useBackAction } from "./mobile";
import { supportsModelDetection } from "./modelTestTypes";
import { DegradationBadge } from "./DegradationMark";
import AccountStatus from "./AccountStatus";

type Props = {
  accounts: Account[];
  online: boolean;
  selected: Set<number>;
  active?: boolean;
  connectionKey?: string;
  select: (id: number, checked: boolean) => void;
  schedule: (account: Account) => ReactNode;
  error: (id: number) => void;
  report: (error: unknown) => void;
  accountActions: (account: Account) => AccountActionItem[];
};

const refreshSnapshot = () => command("refresh");

const MobileAccountCard = memo(function MobileAccountCard({
  account: a,
  online,
  isSelected,
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
  active?: boolean;
  connectionKey?: string;
  select: (id: number, checked: boolean) => void;
  schedule: (account: Account) => ReactNode;
  error: (id: number) => void;
  report: (error: unknown) => void;
  accountActions: (account: Account) => AccountActionItem[];
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
        {schedule(a)}
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
  const active = props.accounts.find((a) => a.id === activeId);
  useBackAction(!!active, () => setActiveId(null));
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
        <div className="modal-backdrop" onClick={() => setActiveId(null)}>
          <section
            className="mobile-action-sheet"
            role="dialog"
            aria-modal="true"
            aria-label="账号操作"
            onClick={(e) => e.stopPropagation()}
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
