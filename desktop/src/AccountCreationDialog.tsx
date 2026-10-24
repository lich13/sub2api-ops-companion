import { useEffect, useRef, useState } from "react";
import { Copy, ExternalLink, LoaderCircle, Plus, X } from "lucide-react";
import { api, command } from "./bridge";
import { useDialogFocus } from "./dialogFocus";
import { useBackAction } from "./mobile";
import type { Account } from "./types";
import AccountCreationFields from "./AccountCreationFields";
import { authConfig, authMethods, authProvider, availableGroups, buildPayload, customErrorRisks, expiryInput, fields, fieldVisible, groupLongContextBilling, importParameters, isOAuth, newDraft, parseExpiry, platforms, reuseDraft, setField, tierOptions, tokenIdentity, typeNames, validateDraft, withProtocolDefaults, type Created, type CreationDraft, type CreationOptions, type RecordValue } from "./accountCreation";
import "./account-settings.css";
import "./account-creation.css";

type Outcome = { index: number; status: "success" | "failed" | "unknown"; message: string };
type Authorization = { auth_url: string; session_id: string; state?: string };
const message = (error: unknown) => String(error).replace(/^Error: /, "");
export const uncertainCreation = (error: unknown) => !/\[4\d\d\]/.test(message(error));

export function AccountCreationButton({ accounts,online,connectionKey,report }: { accounts: Account[]; online: boolean; connectionKey: string; report: (message: string) => void }) {
  const [opened,setOpened] = useState<string|null>(null);
  return <><button disabled={!online} onClick={() => setOpened(connectionKey)}><Plus size={15}/>添加账号</button>{opened === connectionKey && <AccountCreationDialog key={connectionKey} accounts={accounts} online={online} close={() => setOpened(null)} report={report}/>}</>;
}

export default function AccountCreationDialog({ accounts,online,close,report }: { accounts: Account[]; online: boolean; close: () => void; report: (message: string) => void }) {
  const [draft,setDraft] = useState(newDraft), [step,setStep] = useState(1), [options,setOptions] = useState<CreationOptions|null>(null);
  const [query,setQuery] = useState(""), [source,setSource] = useState(""), [reusing,setReusing] = useState(false), [reuseNotice,setReuseNotice] = useState("");
  const [busy,setBusy] = useState(false), [error,setError] = useState(""), [optionError,setOptionError] = useState(""), [editorErrors,setEditorErrors] = useState<Record<string,string>>({});
  const [tls,setTls] = useState<{id:number;name:string}[]>([]), [aiStudio,setAiStudio] = useState(false), [quotaNotify,setQuotaNotify] = useState(false), [webSearch,setWebSearch] = useState(false);
  const [method,setMethod] = useState("manual"), [input,setInput] = useState(""), [authorization,setAuthorization] = useState<Authorization|null>(null), [identity,setIdentity] = useState<ReturnType<typeof tokenIdentity>|null>(null);
  const [results,setResults] = useState<Outcome[]>([]), [mixed,setMixed] = useState(false), [confirmed,setConfirmed] = useState(false);
  const [errorCodesConfirmed,setErrorCodesConfirmed] = useState("");
  const errorRisks=customErrorRisks(draft), errorRiskKey=errorRisks.join(",");
  const [previewModels,setPreviewModels] = useState<string[]>([]), [previewSelected,setPreviewSelected] = useState<string[]>([]);
  const [modelsPreviewed,setModelsPreviewed] = useState(false);
  const scope = useRef(0), mounted = useRef(true), guard = useRef(false), sourceRequest = useRef(0), defaultsTouched = useRef(false);
  const ledger = useRef(new Map<string,Outcome>()), dialog = useRef<HTMLElement>(null), draftRef = useRef(draft), sourceRef = useRef(source);
  draftRef.current = draft; sourceRef.current = source;
  const dismiss = () => { if (!guard.current) close(); };
  useDialogFocus(dialog); useBackAction(true,dismiss);
  const candidates = accounts.filter(a => a.platform === draft.platform && a.type === draft.type);
  const visibleCandidates = candidates.filter(a => `${a.name} ${a.id}`.toLowerCase().includes(query.trim().toLowerCase()));
  useEffect(() => { mounted.current=true; return () => { mounted.current=false; ++scope.current; ++sourceRequest.current; ledger.current.clear(); }; },[]);
  useEffect(() => {
    if (!candidates.some(a => String(a.id) === source)) setSource(candidates.length === 1 ? String(candidates[0].id) : "");
  },[draft.platform,draft.type,accounts,source]);
  async function loadOptions() {
    setOptionError("");
    const requests = await Promise.allSettled([
      api<{features:Record<string,boolean>}>("GET","/capabilities"), api<CreationOptions>("GET","/account-creation/options"),
      api<{id:number;name:string}[]>("GET","/account-creation/catalog/tls"),
      api<{ai_studio_oauth_enabled:boolean}>("GET","/account-creation/catalog/gemini"),
      api<{account_quota_notify_enabled:boolean}>("GET","/account-creation/catalog/settings"),
      api<{enabled:boolean}>("GET","/account-creation/catalog/web-search"),
    ] as const);
    if (!mounted.current) return;
    const [cap,opt,tls,gemini,settings,web]=requests;
    if (cap.status !== "fulfilled" || !cap.value.features.account_creation) { setOptionError("服务器尚不支持添加账号，请更新服务器后重试"); return; }
    if (opt.status === "fulfilled") setOptions(opt.value);
    else setOptionError("账号选项读取失败，请重试");
    if (tls.status === "fulfilled") setTls(tls.value);
    if (gemini.status === "fulfilled") setAiStudio(gemini.value.ai_studio_oauth_enabled);
    if (settings.status === "fulfilled") setQuotaNotify(settings.value.account_quota_notify_enabled);
    if (web.status === "fulfilled") setWebSearch(web.value.enabled);
    if ([tls,gemini,settings,web].some(r => r.status === "rejected")) setOptionError("部分高级选项读取失败，请重试以读取完整选项");
  }
  useEffect(() => { void loadOptions(); },[]);
  useEffect(() => {
    if (draft.platform !== "antigravity") return;
    const current=scope.current;
    void api<Record<string,string>>("GET","/account-creation/catalog/antigravity-models").then(value => {
      if (mounted.current && current === scope.current && !defaultsTouched.current) setDraft(d => setField(d,"credentials.model_mapping",value));
    }).catch(() => { if(mounted.current && current===scope.current)setError("默认模型映射读取失败，可手动填写或切换平台后重试"); });
  },[draft.platform,draft.type]);
  function clearAuth() { setAuthorization(null); setIdentity(null); }
  function switchKind(platform: string,type?: string) {
    ++scope.current; ++sourceRequest.current; defaultsTouched.current=false; setReusing(false); setBusy(false); guard.current=false;
    setDraft(old => ({...newDraft(platform,type),name:old.name,notes:old.notes})); setSource("");setQuery("");setReuseNotice("");setInput("");setMethod("manual");clearAuth();setResults([]);ledger.current.clear();setError("");setEditorErrors({});setMixed(false);setConfirmed(false);setPreviewModels([]);setStep(1);
    setErrorCodesConfirmed("");
    setModelsPreviewed(false);
  }
  function change(path: string,value: unknown) {
    if (path === "credentials.model_mapping") defaultsTouched.current=true;
    setDraft(old => {
      let next=setField(old,path,value);
      if (path === "credentials.account_mode" || path === "credentials.api_protocol") next=withProtocolDefaults(next);
      if (path === "credentials.oauth_type") next=setField(next,"credentials.tier_id",tierOptions(next)[0]);
      return next;
    });
    if (["proxy_id","credentials.oauth_type","credentials.project_id","credentials.tier_id"].includes(path)) clearAuth();
    if (path === "group_ids") { setMixed(false);setConfirmed(false); }
    if (path.startsWith("credentials.custom_error_codes")) setErrorCodesConfirmed("");
    if (["credentials.api_key","credentials.base_url","credentials.api_base_urls"].includes(path)) setModelsPreviewed(false);
    setError("");
  }
  async function reuse() {
    if (!source || !online || guard.current) return;
    const current=scope.current, request=++sourceRequest.current, id=source;
    setReusing(true);setError("");setReuseNotice("");
    try {
      const value=await api<CreationDraft>("GET",`/account-creation/source/${id}`);
      if (!mounted.current || current!==scope.current || request!==sourceRequest.current || sourceRef.current!==id) return;
      const next=reuseDraft(draftRef.current,value);
      if(next.credentials.oauth_type!==draftRef.current.credentials.oauth_type)clearAuth();
      defaultsTouched.current=true;setDraft(next);setEditorErrors({});setMixed(false);setConfirmed(false);setReuseNotice("已复用配置，可继续修改；账号名称与新填凭据已保留");
      setErrorCodesConfirmed("");
      setModelsPreviewed(false);
    } catch(e) { if(mounted.current && current===scope.current && request===sourceRequest.current)setError(message(e)); }
    finally { if(mounted.current && request===sourceRequest.current)setReusing(false); }
  }
  const invalid = () => Object.entries(editorErrors).find(([path,error]) => error && fields.some(f => f.path===path && fieldVisible(f,draft)))?.[1] || validateDraft(draft,step===2 && !isOAuth(draft),!(draft.platform==="grok" && draft.type==="oauth" && (step===1 || method==="grok-sso")));
  async function checkMixed() {
    if (confirmed || !["anthropic","antigravity"].includes(draft.platform)) return true;
    const result=await api<{has_risk:boolean}>("POST","/account-creation/check-mixed-channel",{platform:draft.platform,group_ids:draft.group_ids});
    if(result.has_risk) {setMixed(true);return false;}return true;
  }
  async function generate() {
    if(guard.current || !online)return;
    const current=scope.current;guard.current=true;setBusy(true);setError("");clearAuth();
    try { const result=await api<Authorization>("POST",`/account-creation/auth/${authProvider(draft)}/authorize`,authConfig(draft));if(mounted.current && current===scope.current)setAuthorization({...result,state:result.state || new URL(result.auth_url).searchParams.get("state") || undefined}); }
    catch(e){if(mounted.current && current===scope.current)setError(message(e));}
    finally{guard.current=false;if(mounted.current && current===scope.current)setBusy(false);}
  }
  async function openAuthorization() {
    if (!authorization) return;
    try {await command("open_authorization_url",{url:authorization.auth_url});}catch{setError("无法打开系统浏览器，请复制授权链接后打开");}
  }
  async function preview() {
    if (guard.current || !draft.credentials.api_key) {setError("请先填写 API Key");return;}
    guard.current=true;setBusy(true);setError("");const current=scope.current;
    try {const body=buildPayload(draft);const value=await api<{models:string[];warnings:string[]}>("POST","/account-creation/models",{platform:draft.platform,type:draft.type,base_url:body.credentials.base_url,api_key:body.credentials.api_key,model_mapping:body.credentials.model_mapping});if(mounted.current && current===scope.current){setModelsPreviewed(true);setPreviewModels(value.models);setPreviewSelected(value.models);if(value.warnings?.length)setError(value.warnings.join("；"));}}
    catch(e){if(mounted.current && current===scope.current)setError(message(e));}finally{guard.current=false;if(mounted.current && current===scope.current)setBusy(false);}
  }
  async function authorizedIdentity(item: string) {
    if (method === "manual" && identity) return identity;
    const provider=authProvider(draft), config=authConfig(draft);delete config.project_id;
    let body: RecordValue, action: string;
    if (method === "manual") {
      if (!authorization) throw new Error("请先生成授权链接");
      let code=item.trim(),state=authorization.state;
      try {const url=new URL(code);code=url.searchParams.get("code")||"";state=url.searchParams.get("state")||state;}catch{/* Raw authorization codes are supported. */}
      if (!code) throw new Error("请填写授权码或完整回调地址");
      body={...config,session_id:authorization.session_id,code,...(state?{state}: {})};action="exchange";
    } else if (method === "cookie") {body={...config,code:item};action="cookie";}
    else {body={...config,refresh_token:item,...(method === "mobile-refresh" ? {client_id:"app_LlGpXReQgckcGGUo2JrYvtJK"} : {})};action="refresh";}
    const value=await api<RecordValue>("POST",`/account-creation/auth/${provider}/${action}`,body);
    const resolved=tokenIdentity(draft.platform,value,action === "refresh" ? item : undefined);
    if (!resolved.credentials.access_token) throw new Error("授权未返回可用令牌，请重新授权");
    if(method === "manual")setIdentity(resolved);
    return resolved;
  }
  async function submit() {
    if(guard.current || !online || !options || reusing)return;
    const problem=invalid();if(problem){setError(problem);return;}
    if(errorRiskKey && errorCodesConfirmed!==errorRiskKey){setError("请先确认自定义错误码对调度的影响");return;}
    guard.current=true;setBusy(true);setError("");const current=scope.current;
    try {
      if(!await checkMixed())return;
      if(step===1){setStep(2);return;}
      const nativeImport=["codex-session","agent-identity","codex-pat","grok-sso"].includes(method) && isOAuth(draft);
      const items=!isOAuth(draft) ? ["direct"] : ["manual","codex-session","agent-identity","codex-pat"].includes(method) ? [input.trim()] : input.split(/\r?\n/).map(v=>v.trim()).filter(Boolean);
      if(!items.length || items.some(v=>!v)){setError("请填写凭据");return;}
      const outcomes: Outcome[]=[];
      for(let index=0;index<items.length;index++) {
        if(!mounted.current || current!==scope.current)break;
        const item=items[index], key=`${isOAuth(draft)?method:"direct"}:${item}`, previous=ledger.current.get(key);
        if(previous && previous.status!=="failed"){outcomes.push({...previous,index:index+1});continue;}
        let sent=false;
        try {
          const currentDraft={...draft,name:items.length>1 && draft.name.trim() ? `${draft.name} #${index+1}` : draft.name};
          let detail="已创建",partial=false;
          if(nativeImport) {
            const {credential_extras,...common}=importParameters(currentDraft);sent=true;
            if(method==="grok-sso") {
              const result=await api<{created:{account:{id:number}}[];failed:unknown[]}>("POST","/account-creation/import/grok-sso",{...common,credentials:credential_extras,sso_tokens:[item]});
              if(result.failed.length){sent=false;throw new Error("SSO 导入失败，请检查凭据后重试");}
              if(!result.created.length)throw new Error("SSO 导入未返回创建结果");detail=`已创建 #${result.created[0].account.id}`;
            } else if(method==="codex-pat") {
              const result=await api<Created>("POST","/account-creation/import/codex-pat",{...common,credential_extras,access_token:item});detail=`已创建 #${result.account.id}`;
            } else {
              const result=await api<{created:number;updated:number;skipped:number;failed:number;errors:{index:number;message:string}[];warnings:{index:number;message:string}[]}>("POST","/account-creation/import/codex-session",{...common,credential_extras,content:item,update_existing:true});
              detail=`创建 ${result.created}，更新 ${result.updated}，跳过 ${result.skipped}，失败 ${result.failed}`;
              if(result.errors.length)detail+=`；失败项 ${result.errors.map(e=>e.index).join("、")}，请仅填入失败项重试`;
              if(result.warnings.length)detail+=`；${result.warnings.map(e=>`#${e.index} ${e.message}`).join("；")}`;
              partial=result.failed>0;
            }
          } else {
            const auth=isOAuth(draft) ? await authorizedIdentity(item) : null;
            if(!mounted.current || current!==scope.current)break;
            const payload={...buildPayload(currentDraft,auth?.credentials,auth?.extra),...(confirmed?{confirm_mixed_channel_risk:true}:{})};
            sent=true;const result=await api<Created>("POST",modelsPreviewed?"/account-creation?sync_models=true":"/account-creation",payload);
            detail=`已创建 #${result.account.id}${result.warnings.length?`；${result.warnings.join("；")}`:""}`;
          }
          const outcome:Outcome={index:index+1,status:partial?"unknown":"success",message:detail};ledger.current.set(key,outcome);outcomes.push(outcome);
        } catch(e) {
          const msg=message(e),unknown=sent && uncertainCreation(e);
          if(msg.includes("mixed_channel_warning")){setMixed(true);setConfirmed(false);}
          const outcome:Outcome={index:index+1,status:unknown?"unknown":"failed",message:unknown ? "结果未知，请刷新账号列表核对。此项不会重复提交。" : msg};
          ledger.current.set(key,outcome);outcomes.push(outcome);
        }
        if(mounted.current && current===scope.current)setResults([...outcomes]);
      }
      if(!mounted.current || current!==scope.current)return;
      setResults(outcomes);
      if(outcomes.some(r=>r.status!=="failed"))void command("refresh").catch(()=>{});
      if(outcomes.length && outcomes.every(r=>r.status==="success")){report(outcomes.map(r=>r.message).join("；"));close();}
    }catch(e){if(mounted.current && current===scope.current)setError(message(e));}
    finally{guard.current=false;if(mounted.current && current===scope.current)setBusy(false);}
  }
  const credential = (key: string,label: string,optional=false) => <label key={key}>{label}{optional?"（可选）":""}<input autoComplete="off" spellCheck={false} type="password" value={String(draft.credentials[key]??"")} onChange={e=>change(`credentials.${key}`,e.target.value)}/></label>;
  const methods=authMethods(draft), blockedDirect=ledger.current.get("direct:direct")?.status === "unknown";
  return <div className="modal-backdrop" onClick={dismiss}><section className="account-settings-dialog account-creation-dialog" ref={dialog} role="dialog" aria-modal="true" aria-label="添加账号" onClick={e=>e.stopPropagation()} onKeyDown={e=>{if(e.key==="Escape"){e.stopPropagation();dismiss();}}}>
    <header><div><h2>添加账号</h2><span>{step===1?"1 · 基本信息与配置":"2 · 凭据与授权"}</span></div><button type="button" className="icon-button" aria-label="关闭添加账号" disabled={busy} onClick={dismiss}><X size={18}/></button></header>
    <form id="account-creation-form" className="account-settings-body" onSubmit={e=>{e.preventDefault();void submit();}} autoComplete="off">
      {optionError && <p className="settings-notice bad-text" role="alert">{optionError} <button type="button" disabled={busy} onClick={()=>void loadOptions()}>重新读取选项</button></p>}
      {!options && !optionError && <p role="status">正在读取账号选项…</p>}
      <fieldset disabled={busy || !online} className="creation-form-fields">
        <div hidden={step!==1}>
          <div className="creation-grid"><label>平台<select value={draft.platform} onChange={e=>switchKind(e.target.value)}>{platforms.map(p=><option key={p.id} value={p.id}>{p.label}</option>)}</select></label><label>账号类型<select value={draft.type} onChange={e=>switchKind(draft.platform,e.target.value)}>{platforms.find(p=>p.id===draft.platform)?.types.map(t=><option key={t} value={t}>{draft.platform==="antigravity" && t==="apikey"?"Upstream":typeNames[t]}</option>)}</select></label><label>账号名称<input value={draft.name} maxLength={100} onChange={e=>change("name",e.target.value)}/></label><label>备注<input value={draft.notes??""} onChange={e=>change("notes",e.target.value)}/></label></div>
          <section className="creation-reuse" aria-label="复用账号配置"><strong>复用同平台、同类型账号</strong>{candidates.length>1 && <input aria-label="搜索来源账号" type="search" placeholder="搜索账号名称或 ID" value={query} onChange={e=>setQuery(e.target.value)}/>}<div className="creation-inline"><select aria-label="来源账号" value={source} onChange={e=>{++sourceRequest.current;setReusing(false);setSource(e.target.value);setReuseNotice("");}}><option value="">{candidates.length?"选择来源账号":"暂无可复用账号"}</option>{candidates.filter(a=>visibleCandidates.includes(a) || String(a.id)===source).map(a=><option key={a.id} value={a.id}>{a.name} · #{a.id}</option>)}</select><button type="button" disabled={!source || reusing} onClick={()=>void reuse()}>{reusing?<LoaderCircle size={14} className="spin"/>:<Copy size={14}/>}复用配置</button></div>{reuseNotice && <p role="status">{reuseNotice}</p>}</section>
          <div className="creation-grid"><label>代理<select value={draft.proxy_id??""} onChange={e=>change("proxy_id",e.target.value?Number(e.target.value):null)}><option value="">不使用代理</option>{options?.proxies.map(p=><option key={p.id} value={p.id}>{p.name}{p.status && p.status!=="active"?"（未启用）":""}</option>)}</select></label><label>并发<input type="number" min={1} value={draft.concurrency} onChange={e=>change("concurrency",Number(e.target.value))}/></label><label>负载因子<input type="number" min={1} placeholder="跟随并发数" value={draft.load_factor??""} onChange={e=>change("load_factor",e.target.value?Number(e.target.value):null)}/></label><label>优先级<input type="number" min={0} value={draft.priority} onChange={e=>change("priority",Number(e.target.value))}/></label><label>计费倍率<input type="number" min={0} step="any" value={draft.rate_multiplier} onChange={e=>change("rate_multiplier",Number(e.target.value))}/></label><label>到期时间（本地时区）<input type="datetime-local" value={expiryInput(draft.expires_at)} onChange={e=>change("expires_at",parseExpiry(e.target.value))}/></label></div>
          <fieldset className="creation-choices"><legend>分组</legend>{availableGroups(draft,options?.groups??[]).map(g=><label key={g.id}><input type="checkbox" checked={draft.group_ids.includes(g.id)} onChange={e=>change("group_ids",e.target.checked?[...draft.group_ids,g.id]:draft.group_ids.filter(id=>id!==g.id))}/>{g.name}</label>)}{!draft.group_ids.length && <small className="muted">未选择时采用 Sub2API 的默认分组规则</small>}</fieldset>
          <AccountCreationFields draft={draft} change={change} invalid={(path,error)=>setEditorErrors(old=>({...old,[path]:error}))} tls={tls} aiStudio={aiStudio} quotaNotify={quotaNotify} webSearch={webSearch} groupLongContext={groupLongContextBilling(draft,options?.groups??[])}/>
          {draft.platform==="anthropic" && draft.type==="apikey" && <p className="muted">全局 Web Search 模拟：{webSearch?"开启":"关闭"}</p>}
        </div>
        {step===2 && <>
          <p className="creation-summary">{draft.name} · {platforms.find(p=>p.id===draft.platform)?.label} · {typeNames[draft.type]}</p>
          {!isOAuth(draft) ? <div className="creation-grid">
            {draft.type==="service_account" ? <label className="creation-wide">Service Account JSON<input aria-label="Service Account 文件" type="file" accept=".json,application/json" onChange={async e=>{const file=e.target.files?.[0];e.target.value="";if(!file)return;const current=scope.current;if(file.size>1_000_000){setError("凭据文件过大");return;}try{const text=await file.text();if(mounted.current && current===scope.current)change("credentials.service_account_json",text);}catch{setError("凭据文件读取失败");}}}/><textarea aria-label="Service Account JSON" spellCheck={false} value={String(draft.credentials.service_account_json??"")} onChange={e=>change("credentials.service_account_json",e.target.value)}/></label> : draft.type==="bedrock" && draft.credentials.auth_mode==="sigv4" ? <>{credential("aws_access_key_id","AWS Access Key ID")}{credential("aws_secret_access_key","AWS Secret Access Key")}{credential("aws_session_token","AWS Session Token",true)}</> : credential("api_key","API Key")}
            {draft.type==="apikey" && <div className="creation-wide"><button type="button" onClick={()=>void preview()}>读取上游模型</button>{previewModels.length>0 && <section className="creation-choices" aria-label="上游模型候选">{previewModels.map(model=><label key={model}><input type="checkbox" checked={previewSelected.includes(model)} onChange={e=>setPreviewSelected(old=>e.target.checked?[...old,model]:old.filter(v=>v!==model))}/>{model}</label>)}<button type="button" onClick={()=>{change("credentials.model_mapping",{...(draft.credentials.model_mapping as RecordValue??{}),...Object.fromEntries(previewSelected.map(m=>[m,m]))});setPreviewModels([]);}}>添加所选模型</button></section>}</div>}
          </div> : <>
            <label>添加方式<select value={method} onChange={e=>{setMethod(e.target.value);setInput("");clearAuth();setError("");setResults([]);}}>{methods.map(m=><option key={m.id} value={m.id}>{m.label}</option>)}</select></label>
            {method==="manual" && <div className="creation-authorization"><button type="button" onClick={()=>void generate()}>生成授权链接</button>{authorization && <><input aria-label="授权链接" readOnly value={authorization.auth_url}/><div className="creation-inline"><button type="button" onClick={()=>void openAuthorization()}><ExternalLink size={14}/>系统浏览器授权</button><button type="button" onClick={()=>void navigator.clipboard.writeText(authorization.auth_url).catch(()=>setError("复制失败，请手动复制授权链接"))}><Copy size={14}/>复制链接</button></div></>}</div>}
            <label>{method==="manual"?"授权码或回调地址":methods.find(m=>m.id===method)?.label}<textarea aria-label="授权凭据" spellCheck={false} value={input} onChange={e=>{setInput(e.target.value);setIdentity(null);}}/></label>
            <p className="muted">{["refresh","mobile-refresh","cookie","grok-sso"].includes(method)?"支持批量，每行一个凭据。":"codex-session"===method || method==="agent-identity"?"粘贴原生导出的 JSON；支持批量导入，匹配的已有账号将更新。":"授权完成后粘贴凭据并提交。"}</p>
            {["codex-session","agent-identity"].includes(method) && <label>导入凭据文件<input type="file" multiple accept=".json,.txt,application/json" onChange={async e=>{const files=Array.from(e.target.files??[]);e.target.value="";const current=scope.current;try{if(files.reduce((n,f)=>n+f.size,0)>1_000_000)throw new Error("凭据文件过大");const values=await Promise.all(files.map(f=>f.text()));if(mounted.current && current===scope.current)setInput(values.join("\n"));}catch(err){setError(message(err));}}}/></label>}
          </>}
        </>}
      </fieldset>
      {mixed && <div className="creation-confirm" role="alert"><p>所选分组混用 Claude 与 Antigravity 渠道，可能影响账号风险与调度。确认继续后再提交。</p><label><input type="checkbox" checked={confirmed} disabled={busy} onChange={e=>setConfirmed(e.target.checked)}/>我确认混合渠道风险</label></div>}
      {errorRisks.length>0 && <div className="creation-confirm">{errorRisks.map(code=><p key={code}>将 {code} 加入自定义错误码后，该错误会停止账号调度，并覆盖原生的{code===429?"临时限流":"临时过载"}处理。</p>)}<label><input type="checkbox" checked={errorCodesConfirmed===errorRiskKey} disabled={busy} onChange={e=>setErrorCodesConfirmed(e.target.checked?errorRiskKey:"")}/>确认这些错误码停止账号调度</label></div>}
      {results.length>0 && <ol className="creation-results" aria-label="创建结果">{results.map(r=><li key={r.index} className={r.status==="success"?"":"bad-text"}>#{r.index} · {r.message}</li>)}</ol>}
      {error && <p role="alert" className="bad-text settings-notice">{error}</p>}
      {blockedDirect && <p className="settings-notice">请刷新核对账号是否已经创建，再关闭此窗口。</p>}
    </form>
    <footer><button type="button" disabled={busy} onClick={step===2?()=>{setStep(1);setError("");}:dismiss}>{step===2?"上一步":"取消"}</button>{results.length>0 && <button type="button" disabled={busy} onClick={()=>void command("refresh").catch(()=>setError("刷新失败，请稍后重试"))}>刷新账号列表</button>}<button className="primary" type="submit" form="account-creation-form" disabled={busy || !online || !options || reusing || blockedDirect || mixed && !confirmed}>{busy?<LoaderCircle className="spin" size={15}/>:<Plus size={15}/>} {step===1?"下一步":results.length?"提交未成功项":"创建账号"}</button></footer>
  </section></div>;
}
