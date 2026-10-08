import { useState } from "react";
import { Search } from "lucide-react";
import type { Account } from "./types";

export default function RecoveryAccounts({ accounts, connection, models, change }: {
  accounts: Account[]; connection: number[]; models: number[];
  change: (connection: number[], models: number[]) => void;
}) {
  const [query, setQuery] = useState("");
  const visible = accounts.filter(a => a.platform === "openai" && a.type === "oauth" && a.recovery_selectable !== false
    && `${a.name} #${a.id}`.toLowerCase().includes(query.trim().toLowerCase()));
  return <div className="recovery-selection">
    <label className="search"><Search size={15}/><input aria-label="搜索恢复账号" placeholder="搜索账号或 ID" value={query} onChange={e => setQuery(e.target.value)}/></label>
    <div className="recovery-columns">
      {(["connection", "model"] as const).map(mode => <fieldset key={mode}>
        <legend>{mode === "connection" ? "测试连接" : "模型测试"}</legend>
        <div className="recovery-account-list">
          {visible.map(a => {
            const selected = mode === "connection" ? connection : models;
            return <label className="check-account" key={a.id}>
              <input type="checkbox" checked={selected.includes(a.id)} onChange={e => {
                const target = e.target.checked ? [...selected.filter(id => id !== a.id), a.id] : selected.filter(id => id !== a.id);
                const other = (mode === "connection" ? models : connection).filter(id => !e.target.checked || id !== a.id);
                change(mode === "connection" ? target : other, mode === "model" ? target : other);
              }}/><span>{a.name}<small>#{a.id}</small></span>
            </label>;
          })}
          {!visible.length && <span className="hint">没有匹配的账号</span>}
        </div>
      </fieldset>)}
    </div>
  </div>;
}
