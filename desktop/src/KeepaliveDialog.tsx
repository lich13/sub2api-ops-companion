import { useEffect, useRef, useState } from "react";
import { LoaderCircle, X } from "lucide-react";
import { api, command } from "./bridge";
import { useBackAction } from "./mobile";
import { fullTime, type Account, type AccountKeepalive } from "./types";

const labels: Record<string, string> = {
  disabled: "未启用", waiting: "等待检测", paused: "已暂停", running: "恢复中", needs_confirmation: "待核对",
};
function checked(value: AccountKeepalive, id: number): AccountKeepalive {
  if (!value || value.account_id !== id || typeof value.enabled !== "boolean" || typeof value.read_only !== "boolean"
      || !/^[0-9a-f]{64}$/.test(value.version) || !Number.isSafeInteger(value.generation) || value.generation < 0
      || !Object.prototype.hasOwnProperty.call(labels, value.status) || typeof value.reason !== "string" || typeof value.paused_reason !== "string"
      || !Number.isInteger(value.restore_count) || value.restore_count < 0 || value.restore_count > 5
      || !Number.isInteger(value.budget_used) || value.budget_used < value.restore_count || value.budget_used > 5
      || [value.last_restore_at, value.next_allowed_at].some((at) => at !== null && (typeof at !== "string" || !Number.isFinite(Date.parse(at))))) {
    throw new Error("保活设置响应无效，请重新读取");
  }
  return value;
}
function message(error: unknown) {
  const text = error instanceof Error ? error.message : String(error);
  if (text.includes("账号不存在")) return text;
  return /\b404\b|接口不存在|未知接口/.test(text) ? "当前服务版本不支持账号保活，请先更新服务" : text;
}

export default function KeepaliveDialog({ account, online, connectionKey, close, report }: {
  account: Account; online: boolean; connectionKey: string; close: () => void; report: (error: unknown) => void;
}) {
  const [value, setValue] = useState<AccountKeepalive | null>(null);
  const [draft, setDraft] = useState<AccountKeepalive | null>(null);
  const [loading, setLoading] = useState(online), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const epoch = useRef(0), revision = useRef(0), saving = useRef(false);
  const identity = useRef(""), closeAction = useRef(close);
  closeAction.current = close;
  const closeButton = useRef<HTMLButtonElement>(null);
  const dismiss = () => { ++epoch.current; ++revision.current; closeAction.current(); };
  useBackAction(true, dismiss);

  async function read() {
    if (!online || saving.current) return;
    const current = epoch.current, request = ++revision.current;
    setLoading(true); setError("");
    try {
      const result = checked(await api<AccountKeepalive>("GET", `/accounts/${account.id}/keepalive`), account.id);
      if (current === epoch.current && request === revision.current) { setValue(result); setDraft(result); }
    } catch (e) {
      if (current === epoch.current && request === revision.current) { setError(message(e)); setValue(null); setDraft(null); }
    } finally {
      if (current === epoch.current && request === revision.current) setLoading(false);
    }
  }
  useEffect(() => {
    ++epoch.current; ++revision.current; saving.current = false;
    const nextIdentity = `${connectionKey}:${account.id}`;
    if (identity.current !== nextIdentity) { setValue(null); setDraft(null); identity.current = nextIdentity; }
    setBusy(false); setError(""); setLoading(online);
    if (online) void read();
    return () => { ++epoch.current; ++revision.current; };
  }, [account.id, connectionKey, online]);
  useEffect(() => {
    const previous = document.activeElement;
    closeButton.current?.focus();
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.isComposing) { event.preventDefault(); event.stopPropagation(); dismiss(); }
    };
    document.addEventListener("keydown", escape, true);
    return () => { document.removeEventListener("keydown", escape, true); if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, []);
  useEffect(() => {
    if (!online || !value) return;
    let alive = true, timer: ReturnType<typeof setTimeout>;
    const current = epoch.current;
    const poll = async () => {
      const request = revision.current;
      if (!saving.current) {
        try {
          const result = checked(await api<AccountKeepalive>("GET", `/accounts/${account.id}/keepalive`), account.id);
          if (alive && current === epoch.current && request === revision.current && !saving.current) setValue(result);
        } catch { /* Preserve the last verified state and the user's draft. */ }
      }
      if (alive) timer = setTimeout(() => void poll(), 2000);
    };
    timer = setTimeout(() => void poll(), 2000);
    return () => { alive = false; clearTimeout(timer); };
  }, [account.id, connectionKey, online, !!value]);
  async function save() {
    if (!draft || !value || !online || value.read_only || saving.current) return;
    saving.current = true; setBusy(true); setError("");
    const current = epoch.current, request = ++revision.current;
    try {
      const result = checked(await api<AccountKeepalive>("PUT", `/accounts/${account.id}/keepalive`, {
        enabled: draft.enabled, expected_version: draft.version,
      }), account.id);
      if (current === epoch.current && request === revision.current) {
        setValue(result); setDraft(result); await command("refresh");
      }
    } catch (e) {
      if (current === epoch.current && request === revision.current) { setError(message(e)); report(e); }
    } finally {
      if (current === epoch.current && request === revision.current) { saving.current = false; setBusy(false); }
    }
  }
  const disabled = loading || busy || !online || !value || value.read_only;
  return <div className="modal-backdrop" onClick={dismiss}>
    <section className="model-detection-dialog" role="dialog" aria-modal="true" aria-label="账号保活" onClick={(event) => event.stopPropagation()}>
      <header><div><span className="eyebrow">{account.name} #{account.id}</span><h2>账号保活</h2></div>
        <button ref={closeButton} type="button" className="icon-button" aria-label="关闭账号保活" onClick={dismiss}><X size={19}/></button>
      </header>
      <p className="muted">检测到符合条件的临时不可调度时尝试恢复。原生恢复也会清除模型级限制；不会打开已关闭的调度或解除降智标记。</p>
      {loading && <LoaderCircle className="spin" aria-label="正在读取保活设置"/>}
      {!online && <p className="muted">连接离线，恢复连接后可查看和修改。</p>}
      {draft && value && <>
        <label className="switch-row"><span>启用账号保活</span><input type="checkbox" checked={draft.enabled} disabled={disabled} onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })}/></label>
        <dl className="detection-meta">
          <dt>状态</dt><dd>{labels[value.status]}</dd>
          <dt>最近恢复</dt><dd>{value.last_restore_at ? fullTime(value.last_restore_at) : "暂无记录"}</dd>
          <dt>近一小时</dt><dd>{value.restore_count}/5 次已确认恢复</dd>
          {(value.reason || value.paused_reason) && <><dt>原因</dt><dd>{value.reason || value.paused_reason}</dd></>}
          {value.next_allowed_at && <><dt>等待至</dt><dd>{fullTime(value.next_allowed_at)}</dd></>}
          {value.read_only && <><dt>访问模式</dt><dd>只读，无法修改保活设置</dd></>}
        </dl>
        <p className="muted">两次恢复至少间隔 60 秒，一小时最多 5 次。达到上限后暂停，需检查账号并手动重新开启。</p>
      </>}
      {error && <p className="bad-text" role="alert">{error}</p>}
      <footer>
        <button type="button" disabled={busy || loading || !online} onClick={() => void read()}>重新读取</button>
        <button type="button" className="primary" disabled={disabled || !draft} onClick={() => void save()}>{busy && <LoaderCircle size={15} className="spin"/>}保存</button>
      </footer>
    </section>
  </div>;
}
