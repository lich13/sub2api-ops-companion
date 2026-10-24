import { newDraft, platforms, type CreationDraft, type RecordValue } from "./accountCreation";
import type { Account } from "./types";

export function creationPreview(path: string,body: RecordValue,accounts: Account[],emit: () => void) {
  path=path.split("?")[0];
  if(path==="/account-creation/options") return {groups:platforms.map((p,i)=>({id:i+1,name:p.label,platform:p.id,status:"active"})),proxies:[{id:1,name:"示例代理",status:"active"}]};
  if(path.startsWith("/account-creation/source/")) {
    const account=accounts.find(a=>a.id===Number(path.split("/").pop()));if(!account)throw new Error("来源账号已删除 [404]");
    return {...newDraft(account.platform,account.type),name:undefined,concurrency:account.concurrency??10,priority:account.priority,group_ids:[...account.group_ids],load_factor:account.load_factor??null};
  }
  if(path.endsWith("/catalog/tls")) return [{id:1,name:"默认指纹"}];
  if(path.endsWith("/catalog/gemini")) return {ai_studio_oauth_enabled:true};
  if(path.endsWith("/catalog/grok")) return {password_auth_enabled:false};
  if(path.endsWith("/catalog/settings")) return {account_quota_notify_enabled:true};
  if(path.endsWith("/catalog/web-search")) return {enabled:false};
  if(path.endsWith("/catalog/antigravity-models")) return {"fixture-alias":"fixture-model"};
  if(path.endsWith("/check-mixed-channel")) return {has_risk:false};
  if(path.endsWith("/models")) return {models:["fixture-model","fixture-vision"],warnings:[]};
  if(path.includes("/auth/")) return path.endsWith("/authorize") ? {auth_url:"https://example.invalid/authorize?state=fixture",session_id:"fixture-session",state:"fixture"} : {access_token:"fixture-access-token",refresh_token:"fixture-refresh-token",expires_at:2000000000};
  function create(value: CreationDraft) {
    const id=Math.max(1000,...accounts.map(a=>a.id))+1;
    accounts.push({...accounts[0],id,name:value.name,platform:value.platform,type:value.type,priority:value.priority,concurrency:value.concurrency,load_factor:value.load_factor,group_ids:value.group_ids,status:"active",schedulable:true,available:true,blockers:[],usage_windows:[]});emit();
    return {id,name:value.name,platform:value.platform,type:value.type};
  }
  if(path==="/account-creation") return {account:create(body as CreationDraft),warnings:[]};
  if(path.endsWith("/import/codex-pat")) return {account:create({...body,platform:"openai",type:"oauth"} as CreationDraft),warnings:[]};
  if(path.endsWith("/import/grok-sso")) return {created:[{index:0,account:create({...body,platform:"grok",type:"oauth"} as CreationDraft)}],failed:[]};
  if(path.endsWith("/import/codex-session")){create({...body,platform:"openai",type:"oauth"} as CreationDraft);return {created:1,updated:0,skipped:0,failed:0,errors:[],warnings:[]};}
  throw new Error("不支持的示例操作");
}
