import { useEffect, useId, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { readAccountModels, modelDirectoryRevoked, type AccountModel } from "./accountModels";
import { priorityPlatformName } from "./priorities";
import type { Account } from "./types";

export default function AccountModelField({ label, value, accounts, online, set }: {
  label: string; value: unknown; accounts: Account[]; online: boolean; set: (value: string) => void;
}) {
  const [source, setSource] = useState("");
  const [models, setModels] = useState<AccountModel[]>([]);
  const [loading, setLoading] = useState(false), [error, setError] = useState("");
  const request = useRef(0), list = useId();
  async function read(refresh = false) {
    const current = ++request.current;
    if (!source || !online) { setLoading(false); return; }
    setLoading(true); setError("");
    try { const models = await readAccountModels(Number(source), refresh); if (current === request.current) setModels(models); }
    catch (cause) { if (current === request.current) { if (modelDirectoryRevoked(cause)) setModels([]); setError(String(cause).replace(/^Error: /, "")); } }
    finally { if (current === request.current) setLoading(false); }
  }
  useEffect(() => { void read(); return () => { ++request.current; }; }, [source, online]);
  useEffect(() => { if (source && !accounts.some(account => String(account.id) === source)) { ++request.current; setSource(""); setModels([]); setError(""); } }, [accounts, source]);
  return <div className="model-source-field">
    <label className="field"><span>{label}</span><span className="model-directory-control">
      <input aria-label={label} list={list} value={String(value ?? "")} autoComplete="off" onInput={e => set(e.currentTarget.value)}/>
      <button type="button" className="icon-button" aria-label={`刷新${label}列表`} disabled={!source || !online || loading} onClick={() => void read(true)}><RefreshCw size={16} className={loading ? "spin" : undefined}/></button>
    </span><datalist id={list}>{models.map(model => <option key={model.id} value={model.id}>{model.display_name !== model.id ? model.display_name : undefined}</option>)}</datalist></label>
    <label className="field"><span>来源账号</span><select aria-label={`${label}来源账号`} value={source} disabled={!online} onChange={e => { ++request.current; setModels([]); setError(""); setSource(e.target.value); }}><option value="">选择账号</option>{accounts.map(account => <option key={account.id} value={account.id}>{account.name} · {priorityPlatformName(account.platform)} #{account.id}</option>)}</select></label>
    {error && <p className="bad-text model-source-error" role="alert">{error}</p>}
  </div>;
}
