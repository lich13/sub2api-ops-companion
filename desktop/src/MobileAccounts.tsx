import { AccountOperationStatus } from "./OperationPanel";
import { useState, type ReactNode } from "react";
import { MoreHorizontal, X } from "lucide-react";
import { command } from "./bridge";
import { fullTime, type Account } from "./types";
import SchedulingParameters from "./SchedulingParameters";
import { AccountActionButton, isPrimaryAction, type AccountActionItem } from "./AccountActions";
import UsageCell from "./UsageCell";
import { useBackAction } from "./mobile";
import { DegradationBadge } from "./DegradationMark";
import AccountStatus from "./AccountStatus";

type Props = {
  accounts: Account[]; online: boolean; selected: Set<number>; active?: boolean; connectionKey?: string;
  select: (id: number, checked: boolean) => void;
  schedule: (account: Account) => ReactNode;
  error: (id: number) => void;
  report: (error: unknown) => void; accountActions: (account: Account) => AccountActionItem[];
};

export default function MobileAccounts(props: Props) {
  const [activeId, setActiveId] = useState<number | null>(null);
  const active = props.accounts.find((a) => a.id === activeId);
  useBackAction(!!active, () => setActiveId(null));
  return <>
    <div className="mobile-accounts">
      {props.accounts.map((a) => <article className="mobile-account" key={a.id}>
        <header>
          <label className="account-check"><input type="checkbox" aria-label={`选择 ${a.name}`} checked={props.selected.has(a.id)} disabled={!props.online} onChange={(e) => props.select(a.id, e.target.checked)}/></label>
          <div className="mobile-identity"><strong title={a.name}>{a.name}</strong><span>{a.platform === "openai" ? "OpenAI" : a.platform === "grok" ? "Grok" : a.platform} · {a.type === "oauth" ? "OAuth" : a.type === "apikey" ? "Key" : a.type}{a.managed && " · 回退托管"}</span></div>
          {props.schedule(a)}
          <button className="icon-button" aria-label={`${a.name}操作`} onClick={() => setActiveId(a.id)}><MoreHorizontal size={21}/></button>
        </header>
        <div className="mobile-account-status">
          <DegradationBadge account={a}/><AccountOperationStatus id={a.id}/>
          <AccountStatus account={a} onError={props.error}/>
        </div>
        <UsageCell account={a} online={props.online} active={props.active} connectionKey={props.connectionKey} refresh={() => command("refresh")} report={props.report}/>
        <footer>
          {a.last_error_id ? <button className="mobile-error" onClick={() => props.error(a.last_error_id!)}><span className="bad-text">{a.last_error_code || a.last_error_status || "上游错误"}</span><time>{fullTime(a.last_error_at)}</time>{a.success_after_error && <span className="good-text">之后已成功</span>}</button> : <span className="muted">{a.error_message ? "错误时间未知" : "暂无错误记录"}</span>}
          <span className="priority-value">优先级 {a.priority}</span>
        </footer>
        {a.platform === "openai" && ["oauth", "apikey"].includes(a.type) && <div className="mobile-primary-actions">
          {props.accountActions(a).filter(isPrimaryAction).map((item) => <AccountActionButton key={item.id} item={item}/>)}
        </div>}
      </article>)}
    </div>
    {active && <div className="modal-backdrop" onClick={() => setActiveId(null)}><section className="mobile-action-sheet" role="dialog" aria-modal="true" aria-label="账号操作" onClick={(e) => e.stopPropagation()}>
      <header><h2>{active.name}</h2><button className="icon-button" aria-label="关闭账号操作" onClick={() => setActiveId(null)}><X size={20}/></button></header>
      <SchedulingParameters account={active} online={props.online}/>
      <div className="mobile-more-actions">{props.accountActions(active).filter((item) => !isPrimaryAction(item)).map((item) =>
        <AccountActionButton key={item.id} item={item} after={() => setActiveId(null)}/>)}</div>
    </section></div>}
  </>;
}
