import { useEffect, useRef, useState } from "react";
import { Copy, ExternalLink, Link2, LoaderCircle, X } from "lucide-react";
import { api, command } from "./bridge";
import { tokenIdentity, type RecordValue } from "./accountCreation";
import { useBackAction } from "./mobile";
import { useDialogFocus } from "./dialogFocus";
import type { Account } from "./types";
import "./reauthorize-account.css";

type Authorization = { auth_url: string; session_id: string; state?: string; proxy_id?: number };

export default function ReauthorizeAccountDialog({ account, online, close, report }: {
  account: Account; online: boolean; close: () => void; report: (message: unknown) => void;
}) {
  const [authorization, setAuthorization] = useState<Authorization | null>(null);
  const [code, setCode] = useState(""), [busy, setBusy] = useState(false), [submitted, setSubmitted] = useState(false);
  const [error, setError] = useState(""), [copied, setCopied] = useState(false);
  const alive = useRef(false), guard = useRef(false);
  const dialog = useRef<HTMLElement>(null);
  useDialogFocus(dialog);
  useBackAction(true, close);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);

  async function generate() {
    if (!online || guard.current) return;
    guard.current = true; setBusy(true); setError(""); setCode(""); setAuthorization(null); setSubmitted(false); setCopied(false);
    try {
      const settings = await api<{ proxy_id: number | null }>("GET", `/accounts/${account.id}/settings`);
      if (!alive.current) return;
      const proxy = settings.proxy_id ? { proxy_id: settings.proxy_id } : {};
      const result = await api<Authorization>("POST", "/account-creation/auth/openai/authorize", proxy);
      if (alive.current) setAuthorization({ ...result, ...proxy, state: result.state || new URL(result.auth_url).searchParams.get("state") || undefined });
    } catch { if (alive.current) setError("无法生成授权链接，请重新尝试"); }
    finally { guard.current = false; if (alive.current) setBusy(false); }
  }
  async function open() {
    if (!authorization) return;
    try { await command("open_authorization_url", { url: authorization.auth_url }); }
    catch { if (alive.current) setError("无法打开浏览器，请复制授权链接后打开"); }
  }
  async function copy() {
    if (!authorization) return;
    try { await navigator.clipboard.writeText(authorization.auth_url); if (alive.current) setCopied(true); }
    catch { if (alive.current) setError("复制失败，请手动复制授权链接"); }
  }
  async function submit() {
    if (!online || guard.current || !authorization || submitted || !code.trim()) return;
    let value = code.trim(), state = authorization.state;
    try {
      const callback = new URL(value);
      value = callback.searchParams.get("code") || "";
      const returnedState = callback.searchParams.get("state");
      if (returnedState && state && returnedState !== state) { setError("回调地址不属于本次授权，请重新粘贴"); return; }
      state = returnedState || state;
    } catch { /* Raw authorization codes are also accepted. */ }
    if (!value) { setError("请填写授权码或完整回调地址"); return; }
    guard.current = true; setBusy(true); setError(""); setSubmitted(true); setCode("");
    let applying = false;
    try {
      const token = await api<RecordValue>("POST", "/account-creation/auth/openai/exchange", {
        session_id: authorization.session_id, code: value,
        ...(state ? { state } : {}), ...(authorization.proxy_id ? { proxy_id: authorization.proxy_id } : {}),
      });
      if (!alive.current) return;
      const identity = tokenIdentity("openai", token);
      if (!identity.credentials.access_token) throw new Error("missing_token");
      applying = true;
      const result = await api<{ verified: boolean }>("POST", `/accounts/${account.id}/apply-oauth-credentials`, identity);
      if (!alive.current) return;
      if (!result.verified) throw new Error("unconfirmed");
      report(`${account.name}：重新授权完成`);
      void command("refresh").catch(() => {});
      close();
    } catch {
      if (alive.current) setError(applying ? "授权保存结果未确认，请刷新账号核对；未重放请求" : "授权未完成，请重新生成链接后授权");
    } finally {
      guard.current = false;
      if (alive.current) { setAuthorization(null); setBusy(false); }
    }
  }
  return <div className="modal-backdrop" onClick={() => { if (!busy) close(); }}>
    <section ref={dialog} className="reauthorize-dialog" role="dialog" aria-modal="true" aria-label="重新授权" tabIndex={-1} onClick={e => e.stopPropagation()} onKeyDown={e => { if (e.key === "Escape" && !e.nativeEvent.isComposing) { e.preventDefault(); e.stopPropagation(); if (!busy) close(); } }}>
      <header><div className="reauthorize-heading"><Link2 size={18}/><h2>重新授权</h2></div><button className="icon-button" aria-label="关闭重新授权" onClick={close}><X size={18}/></button></header>
      <div className="reauthorize-body">
        <div className="reauthorize-account" title={account.name}>{account.name}</div>
        <button disabled={!online || busy} onClick={() => void generate()}>{busy && !submitted ? <LoaderCircle size={16} className="spin"/> : <Link2 size={16}/>}生成授权链接</button>
        {authorization && <>
          <label>授权链接<input readOnly value={authorization.auth_url} autoComplete="off" spellCheck={false}/></label>
          <div className="reauthorize-links"><button disabled={!online || busy} onClick={() => void open()}><ExternalLink size={16}/>浏览器授权</button><button disabled={busy} onClick={() => void copy()}><Copy size={16}/>{copied ? "已复制" : "复制链接"}</button></div>
          <label>授权码或回调地址<textarea value={code} onChange={e => setCode(e.target.value)} disabled={busy || submitted} autoComplete="off" spellCheck={false} rows={3}/></label>
        </>}
        {error && <p className="reauthorize-error" role="alert">{error}</p>}
      </div>
      <footer><button onClick={close}>取消</button><button className="primary" disabled={!online || busy || submitted || !authorization || !code.trim()} onClick={() => void submit()}>{busy && submitted ? <LoaderCircle size={16} className="spin"/> : <Link2 size={16}/>}完成授权</button></footer>
    </section>
  </div>;
}
