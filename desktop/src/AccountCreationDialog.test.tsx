// @vitest-environment jsdom
import { act } from "react";
import { createRoot,type Root } from "react-dom/client";
import { afterEach,beforeEach,expect,it,vi } from "vitest";
import AccountCreationDialog,{AccountCreationButton} from "./AccountCreationDialog";
import { api,command } from "./bridge";
import { newDraft } from "./accountCreation";
import type { Account } from "./types";
vi.mock("./bridge",()=>({api:vi.fn(),command:vi.fn()}));
vi.mock("./mobile",()=>({useBackAction:vi.fn()}));
Object.assign(globalThis,{IS_REACT_ACT_ENVIRONMENT:true});
let root:Root,container:HTMLDivElement;
const close=vi.fn(),report=vi.fn(), account=(id=1,platform="openai",type="apikey")=>({id,name:`Fixture ${id}`,platform,type,group_ids:[],priority:1,status:"active",schedulable:true,available:true,blockers:[],version:"fixture",last_success_at:null,last_error_at:null,last_error_id:null,last_error_code:null,last_error_status:null,error_message:"",success_after_error:false,usage_windows:[]} as Account);
const button=(name:string)=>[...container.querySelectorAll<HTMLButtonElement>("button")].find(b=>b.textContent?.trim()===name)!;
const field=(name:string)=>[...container.querySelectorAll("label")].find(l=>l.firstChild?.textContent===name || l.textContent?.trim()===name)?.querySelector<HTMLInputElement|HTMLSelectElement|HTMLTextAreaElement>("input,select,textarea")!;
async function click(name:string){await act(async()=>button(name).click());}
async function change(el:HTMLInputElement|HTMLSelectElement|HTMLTextAreaElement,value:string){await act(async()=>{const proto=el instanceof HTMLSelectElement?HTMLSelectElement.prototype:el instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(proto,"value")!.set!.call(el,value);el.dispatchEvent(new Event(el instanceof HTMLSelectElement?"change":"input",{bubbles:true}));});}
async function render(accounts=[account()]){await act(async()=>root.render(<AccountCreationDialog accounts={accounts} online close={close} report={report}/>));}
async function apiKeyStep(){await change(field("平台"),"openai");await change(field("账号类型"),"apikey");await change(field("账号名称"),"New fixture");await click("下一步");await change(field("API Key"),"fixture-secret");}
const baseline=async(_method:string,path:string,body?:unknown):Promise<never>=>{
  if(path==="/capabilities")return {features:{account_creation:true}} as never;
  if(path.endsWith("/options"))return {groups:[{id:1,name:"OpenAI",platform:"openai"}],proxies:[]} as never;
  if(path.endsWith("/catalog/tls"))return [] as never;
  if(path.endsWith("/catalog/gemini"))return {ai_studio_oauth_enabled:false} as never;
  if(path.endsWith("/catalog/settings"))return {account_quota_notify_enabled:true} as never;
  if(path.endsWith("/catalog/web-search"))return {enabled:false} as never;
  if(path.endsWith("/check-mixed-channel"))return {has_risk:false} as never;
  if(path.includes("/source/"))return {...newDraft("openai","apikey"),priority:0,rate_multiplier:0,extra:{quota_limit:0}} as never;
  if(path.endsWith("/authorize"))return {auth_url:"https://example.invalid/authorize?state=fixture",session_id:"fixture-session"} as never;
  if(path.includes("/auth/"))return {access_token:"fixture-access",refresh_token:"fixture-refresh"} as never;
  if(path==="/account-creation")return {account:{id:100,name:(body as {name:string}).name,platform:"openai",type:"apikey"},warnings:[]} as never;
  throw new Error(`unexpected fixture route: ${path}`);
};
beforeEach(()=>{vi.clearAllMocks();container=document.createElement("div");document.body.append(container);root=createRoot(container);vi.mocked(api).mockImplementation(baseline);vi.mocked(command).mockResolvedValue(undefined as never);});
afterEach(async()=>{await act(async()=>root.unmount());container.remove();});

it("shows native field conditions for shared group billing, global web search and Grok headers",async()=>{
  vi.mocked(api).mockImplementation(async(m,p,b)=>p.endsWith("/options")?{groups:[{id:1,name:"Shared billing",platform:"composite",long_context_pricing_enabled:true}],proxies:[]} as never:baseline(m,p,b));
  await render([]);await change(field("账号类型"),"apikey");
  expect(container.textContent).not.toContain("跟随全局");
  await change(field("平台"),"openai");expect(field("长上下文计费")).toBeTruthy();
  await act(async()=>field("Shared billing").click());expect(container.textContent).not.toContain("长上下文计费");
  await change(field("平台"),"grok");expect(field("请求头覆写")).toBeTruthy();expect(button("CLI")).toBeTruthy();
});

it("selects a sole matching source, retains new credentials and creates without writing a local draft",async()=>{
  const storage=vi.spyOn(Storage.prototype,"setItem");await render();await apiKeyStep();await click("上一步");
  expect(container.querySelector<HTMLSelectElement>('[aria-label="来源账号"]')!.value).toBe("1");
  await click("复用配置");expect((field("账号名称") as HTMLInputElement).value).toBe("New fixture");expect((field("优先级") as HTMLInputElement).value).toBe("0");
  await click("下一步");expect((field("API Key") as HTMLInputElement).value).toBe("fixture-secret");await click("创建账号");
  const payload=vi.mocked(api).mock.calls.find(([m,p])=>m==="POST" && p==="/account-creation")?.[2];
  expect(payload).toMatchObject({name:"New fixture",priority:0,rate_multiplier:0,credentials:{api_key:"fixture-secret"},extra:{quota_limit:0}});
  expect(command).toHaveBeenCalledWith("refresh");expect(close).toHaveBeenCalledTimes(1);expect(storage).not.toHaveBeenCalled();storage.mockRestore();
});
it("ignores a source response after a platform switch and keeps drafts after read failure",async()=>{
  let resolve!:(value:unknown)=>void;
  vi.mocked(api).mockImplementation((m,p,b)=>p.includes("/source/")?new Promise(r=>{resolve=r;}) as never:baseline(m,p,b));
  await render();await change(field("平台"),"openai");await change(field("账号类型"),"apikey");await change(field("账号名称"),"New");
  await click("复用配置");await change(field("平台"),"gemini");await act(async()=>resolve({...newDraft("openai","apikey"),priority:99}));
  expect((field("优先级") as HTMLInputElement).value).toBe("1");expect((field("平台") as HTMLSelectElement).value).toBe("gemini");
  await change(field("平台"),"openai");await change(field("账号类型"),"apikey");
  vi.mocked(api).mockImplementation((m,p,b)=>p.includes("/source/")?Promise.reject(new Error("来源账号已删除 [404]")):baseline(m,p,b));await click("复用配置");expect(container.textContent).toContain("来源账号已删除");expect((field("账号名称") as HTMLInputElement).value).toBe("New");
});
it("requires search selection for multiple sources and offers only matching type candidates",async()=>{
  await render([account(1),account(2),account(3,"gemini"),account(4,"openai","oauth")]);await change(field("平台"),"openai");await change(field("账号类型"),"apikey");
  const select=container.querySelector<HTMLSelectElement>('[aria-label="来源账号"]')!;expect(select.value).toBe("");expect(select.options).toHaveLength(3);
  await change(container.querySelector<HTMLInputElement>('[aria-label="搜索来源账号"]')!,"Fixture 2");expect(select.options).toHaveLength(2);await change(select,"2");await click("复用配置");expect(api).toHaveBeenCalledWith("GET","/account-creation/source/2");
});
it("opens OAuth in the system browser and supplies the state extracted from its URL",async()=>{
  await render([]);await change(field("平台"),"openai");await change(field("账号名称"),"New");await click("下一步");await click("生成授权链接");await click("系统浏览器授权");
  expect(command).toHaveBeenCalledWith("open_authorization_url",{url:"https://example.invalid/authorize?state=fixture"});
  await change(container.querySelector<HTMLTextAreaElement>('[aria-label="授权凭据"]')!,"fixture-code");await click("创建账号");
  expect(api).toHaveBeenCalledWith("POST","/account-creation/auth/openai/exchange",{session_id:"fixture-session",code:"fixture-code",state:"fixture"});expect(close).toHaveBeenCalled();
});
it.each(["anthropic","openai","antigravity","grok"])("handles batch authorization and retries only known failures for %s",async(platform)=>{
  let created=0;
  vi.mocked(api).mockImplementation(async(m,p,b)=>{if(p.endsWith("/catalog/antigravity-models"))return {} as never;if(p==="/account-creation"){created++;if(created===2)throw new Error("fixture invalid account [422]");}return baseline(m,p,b);});
  await render([]);await change(field("平台"),platform);await change(field("账号名称"),"Batch");await click("下一步");await change(field("添加方式"),platform==="anthropic"?"cookie":"refresh");await change(container.querySelector<HTMLTextAreaElement>('[aria-label="授权凭据"]')!,"fixture-first\nfixture-second");
  await click("创建账号");expect(close).not.toHaveBeenCalled();expect(created).toBe(2);await click("提交未成功项");expect(created).toBe(3);expect(close).toHaveBeenCalledTimes(1);
  const route=`/account-creation/auth/${platform}/${platform==="anthropic"?"cookie":"refresh"}`;
  expect(vi.mocked(api).mock.calls.filter(([,p])=>p===route)).toHaveLength(3);
  if(platform==="anthropic")expect(api).toHaveBeenCalledWith("POST",route,{code:"fixture-first"});
});
it("keeps unknown creation outcomes without replaying writes",async()=>{
  vi.mocked(api).mockImplementation((m,p,b)=>p==="/account-creation"?Promise.reject(new Error("连接超时，未重放操作")):baseline(m,p,b));
  await render();await apiKeyStep();await click("创建账号");expect(container.textContent).toContain("结果未知");expect(close).not.toHaveBeenCalled();expect(button("提交未成功项").disabled).toBe(true);expect(vi.mocked(api).mock.calls.filter(([,p])=>p==="/account-creation")).toHaveLength(1);
});
it("holds partial native imports and does not repeat successful entries",async()=>{
  vi.mocked(api).mockImplementation(async(m,p,b)=>p.endsWith("/import/codex-session")?{created:1,updated:0,skipped:0,failed:1,errors:[{index:2,message:"fixture failure"}],warnings:[]} as never:baseline(m,p,b));
  await render([]);await change(field("平台"),"openai");await change(field("账号名称"),"Import");await click("下一步");await change(field("添加方式"),"agent-identity");await change(container.querySelector<HTMLTextAreaElement>('[aria-label="授权凭据"]')!,"fixture-content");await click("创建账号");expect(container.textContent).toContain("失败项 2");await click("提交未成功项");expect(vi.mocked(api).mock.calls.filter(([,p])=>p.endsWith("/import/codex-session"))).toHaveLength(1);expect(close).not.toHaveBeenCalled();
});
it("requires mixed-channel confirmation and closes the draft when its connection changes",async()=>{
  vi.mocked(api).mockImplementation(async(m,p,b)=>p.endsWith("/check-mixed-channel")?{has_risk:true} as never:baseline(m,p,b));
  await act(async()=>root.render(<AccountCreationButton accounts={[]} online connectionKey="one" report={report}/>));await click("添加账号");await change(field("账号名称"),"New");await click("下一步");expect(container.textContent).toContain("我确认混合渠道风险");expect(button("下一步").disabled).toBe(true);
  await act(async()=>root.render(<AccountCreationButton accounts={[]} online connectionKey="two" report={report}/>));expect(container.querySelector('[role="dialog"]')).toBeNull();
});
it("requires the native 429 and 529 confirmation after reusing custom error codes",async()=>{
  vi.mocked(api).mockImplementation(async(m,p,b)=>p.includes("/source/")?{...newDraft("openai","apikey"),credentials:{custom_error_codes_enabled:true,custom_error_codes:[429,529]}} as never:baseline(m,p,b));
  await render();await change(field("平台"),"openai");await change(field("账号类型"),"apikey");await change(field("账号名称"),"Fixture");await click("复用配置");await click("下一步");
  expect(container.textContent).toContain("请先确认自定义错误码");expect(container.textContent).toContain("429");expect(container.textContent).toContain("529");
  await act(async()=>[...container.querySelectorAll("label")].find(l=>l.textContent?.includes("确认这些错误码"))!.querySelector<HTMLInputElement>("input")!.click());await click("下一步");expect(field("API Key")).toBeTruthy();
});
it("retains model preview synchronization even with an unrestricted model list",async()=>{
  vi.mocked(api).mockImplementation(async(m,p,b)=>p==="/account-creation/models"?{models:["fixture-model"],warnings:[]} as never:baseline(m,p.split("?")[0],b));
  await render();await apiKeyStep();await click("上一步");await click("不限制模型");await click("下一步");await click("读取上游模型");await click("创建账号");
  expect(vi.mocked(api).mock.calls.find(([,p])=>p==="/account-creation?sync_models=true")?.[2]).toMatchObject({credentials:{model_mapping:{}}});expect(close).toHaveBeenCalled();
});
it.each(["codex-pat","grok-sso","mobile-refresh"])("uses the native %s route and correct payload",async(method)=>{
  vi.mocked(api).mockImplementation(async(m,p,b)=>p.endsWith("/import/grok-sso")?{created:[{account:{id:7}}],failed:[]} as never:p.endsWith("/import/codex-pat")?{account:{id:8},warnings:[]} as never:baseline(m,p,b));
  await render([]);await change(field("平台"),method==="grok-sso"?"grok":"openai");if(method!=="grok-sso")await change(field("账号名称"),"Fixture");await click("下一步");await change(field("添加方式"),method);await change(container.querySelector<HTMLTextAreaElement>('[aria-label="授权凭据"]')!,"fixture-input");await click("创建账号");
  if(method==="grok-sso")expect(api).toHaveBeenCalledWith("POST","/account-creation/import/grok-sso",expect.objectContaining({name:"",sso_tokens:["fixture-input"],credentials:expect.any(Object)}));
  else if(method==="codex-pat")expect(api).toHaveBeenCalledWith("POST","/account-creation/import/codex-pat",expect.objectContaining({access_token:"fixture-input",credential_extras:expect.any(Object)}));
  else expect(api).toHaveBeenCalledWith("POST","/account-creation/auth/openai/refresh",{refresh_token:"fixture-input",client_id:"app_LlGpXReQgckcGGUo2JrYvtJK"});
  expect(close).toHaveBeenCalled();
});
