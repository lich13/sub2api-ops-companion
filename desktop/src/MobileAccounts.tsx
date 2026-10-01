import { AccountOperationStatus } from "./OperationPanel";
import { useState, type ReactNode } from "react";
import { MoreHorizontal, X } from "lucide-react";
import { command } from "./bridge";
import { fullTime, type Account } from "./types";
import { QualityBadge } from "./AccountQuality";
import { PriorityEditor } from "./AccountControls";
import { RecoverStateButton } from "./AccountManagement";
import UsageCell from "./UsageCell";
import { useBackAction } from "./mobile";
import DegradationAction, { DegradationBadge } from "./DegradationMark";

type Props = {
  accounts: Account[]; online: boolean; selected: Set<number>;
  select: (id: number, checked: boolean) => void;
  schedule: (account: Account) => ReactNode;
  quality: (account: Account) => void; test: (account: Account) => void;
  remove: (account: Account) => void; error: (id: number) => void;
  report: (error: unknown) => void; modelTest: (account: Account) => void;
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
          <span className={a.available ? "good-text" : ""}>{a.available ? "可调度" : a.blockers.map((b) => b.label).join(" / ")}</span>
          {["openai", "grok"].includes(a.platform) && ["oauth", "apikey"].includes(a.type) && <QualityBadge value={a.quality} onClick={() => props.quality(a)}/>}
        </div>
        {a.blockers.filter((b) => b.code === "rate_limit_reset_at").map((b) => <div className="mobile-limit" key={b.code}>预计解除 {b.until ? fullTime(b.until) : "时间未知"}</div>)}
        <UsageCell account={a} online={props.online} refresh={() => command("refresh")} report={props.report}/>
        <footer>
          {a.last_error_id ? <button className="mobile-error" onClick={() => props.error(a.last_error_id!)}><span className="bad-text">{a.last_error_code || a.last_error_status || "上游错误"}</span><time>{fullTime(a.last_error_at)}</time>{a.success_after_error && <span className="good-text">之后已成功</span>}</button> : <span className="muted">{a.error_message ? "错误时间未知" : "暂无错误记录"}</span>}
          <button className="text-button" onClick={() => setActiveId(a.id)}>优先级 {a.priority}</button>
        </footer>
      </article>)}
    </div>
    {active && <div className="modal-backdrop" onClick={() => setActiveId(null)}><section className="mobile-action-sheet" role="dialog" aria-modal="true" aria-label="账号操作" onClick={(e) => e.stopPropagation()}>
      <header><h2>{active.name}</h2><button className="icon-button" aria-label="关闭账号操作" onClick={() => setActiveId(null)}><X size={20}/></button></header>
      <div className="field"><span>优先级</span><PriorityEditor account={active} online={props.online} report={props.report}/></div>
      <button disabled={!props.online || !["openai", "grok"].includes(active.platform) || !["oauth", "apikey"].includes(active.type)} onClick={() => { props.test(active); setActiveId(null); }}>测试连接</button>
      {active.platform === "openai" && ["oauth", "apikey"].includes(active.type) && <button disabled={!props.online} onClick={() => { props.modelTest(active); setActiveId(null); }}>模型测试</button>}
      <RecoverStateButton account={active} online={props.online} report={props.report}/>
      <DegradationAction account={active} online={props.online} report={props.report}/>
      <button className="danger-text" disabled={!props.online} onClick={() => { props.remove(active); setActiveId(null); }}>删除账号</button>
    </section></div>}
  </>;
}
