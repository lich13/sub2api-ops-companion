import { describe,expect,it } from "vitest";
import { authConfig,authMethods,authProvider,availableGroups,buildPayload,fields,fieldVisible,groupLongContextBilling,importParameters,newDraft,platforms,reuseDraft,setField,tokenIdentity,validateDraft,withProtocolDefaults } from "./accountCreation";

describe("Sub2API creation payloads",()=>{
  it("limits model configuration to native account types and includes Grok OAuth header overrides",()=>{
    for(const [platform,type] of [["anthropic","oauth"],["anthropic","setup-token"],["gemini","oauth"]]) {
      const draft=newDraft(platform,type);draft.credentials.model_mapping={alias:"fixture-model"};
      expect(buildPayload(draft).credentials.model_mapping).toBeUndefined();
      expect(newDraft(platform,type).credentials.model_mapping).toBeUndefined();
    }
    const draft=newDraft("grok","oauth");draft.name="Fixture";draft.credentials.access_token="fixture-new";
    const source=newDraft("grok","oauth");Object.assign(source.credentials,{access_token:"fixture-old",header_override_enabled:true,header_overrides:{"X-Fixture":""}});
    const reused=reuseDraft(draft,source);
    expect(reused.credentials.access_token).toBe("fixture-new");
    expect(buildPayload(reused).credentials).toMatchObject({header_override_enabled:true,header_overrides:{"X-Fixture":""}});
  });
  it("offers composite groups and gates cross-platform groups and account billing by native conditions",()=>{
    const groups=[{id:1,name:"Claude",platform:"anthropic",long_context_pricing_enabled:true},{id:2,name:"Gemini",platform:"gemini"},{id:3,name:"Antigravity",platform:"antigravity"},{id:4,name:"Composite",platform:"composite",long_context_pricing_enabled:true}];
    const draft=newDraft("antigravity");
    expect(availableGroups(draft,groups).map(g=>g.id)).toEqual([3,4]);
    draft.extra.mixed_scheduling=true;expect(availableGroups(draft,groups)).toEqual(groups);
    expect(groupLongContextBilling(draft,groups)).toBe(false);
    draft.group_ids=[1,4];expect(groupLongContextBilling(draft,groups)).toBe(true);
    draft.group_ids=[1,2];expect(groupLongContextBilling(draft,groups)).toBe(false);
    draft.group_ids=[1,999];expect(groupLongContextBilling(draft,groups)).toBe(false);
  });
  it.each(platforms.flatMap(p=>p.types.map(type=>[p.id,type] as const)))("assembles defaults and credentials for %s / %s",(platform,type)=>{
    const draft=newDraft(platform,type);draft.name="Fixture";
    if(platform==="antigravity" && type==="apikey")draft.credentials.base_url="https://example.invalid";
    if(type==="service_account")draft.credentials.service_account_json=JSON.stringify({project_id:"fixture",client_email:"fixture@example.invalid",private_key:"fixture-private-key"});
    else if(type==="bedrock")Object.assign(draft.credentials,{aws_access_key_id:"fixture-id",aws_secret_access_key:"fixture-secret"});
    else if(type==="apikey")draft.credentials.api_key="fixture-key";
    expect(validateDraft(draft)).toBe("");
    const value=buildPayload(draft,type==="oauth"||type==="setup-token"?{access_token:"fixture-access"}:{});
    expect(value).toMatchObject({platform,type,concurrency:platform==="grok"?1:10,priority:1,rate_multiplier:1,proxy_id:null,load_factor:null,expires_at:null,auto_pause_on_expired:true});
    if(type==="apikey")expect(value.credentials.api_key).toBe("fixture-key");
    if(type==="service_account")expect(value.credentials).toMatchObject({location:"global",tier_id:"vertex",project_id:"fixture",client_email:"fixture@example.invalid"});
    if(type==="bedrock")expect(value.credentials).toMatchObject({auth_mode:"sigv4",aws_region:"us-east-1"});
  });
  it("reuses all declared config while retaining the new name and identity, including false, zero, null and nested values",()=>{
    const current=newDraft("openai","apikey");current.name="New";current.credentials.api_key="fixture-new";
    const source=newDraft("openai","apikey");source.name="Old";source.priority=0;source.rate_multiplier=0;source.notes=null;source.load_factor=null;source.group_ids=[2];source.auto_pause_on_expired=false;
    Object.assign(source.credentials,{api_key:"fixture-old",pool_mode:false,pool_mode_retry_count:0,model_mapping:{alias:"fixture-model"},header_overrides:{"X-Fixture":""},temp_unschedulable_rules:[{error_code:429,keywords:["fixture"],duration_minutes:2,description:""}]});
    Object.assign(source.extra,{quota_limit:0,quota_daily_reset_hour:0,openai_passthrough:false,quota_used:99,sub2ops_detection:true,email:"fixture@example.invalid"});
    const reused=reuseDraft(current,source);
    expect(reused).toMatchObject({name:"New",priority:0,rate_multiplier:0,notes:null,load_factor:null,group_ids:[2],auto_pause_on_expired:false});
    expect(reused.credentials).toMatchObject({api_key:"fixture-new",pool_mode:false,pool_mode_retry_count:0,model_mapping:{alias:"fixture-model"},header_overrides:{"X-Fixture":""}});
    expect(reused.extra).toMatchObject({quota_limit:0,quota_daily_reset_hour:0,openai_passthrough:false});
    expect(reused.extra.quota_used).toBeUndefined();expect(reused.extra.sub2ops_detection).toBeUndefined();expect(reused.extra.email).toBeUndefined();
    (source.credentials.model_mapping as Record<string,string>).alias="changed";expect((reused.credentials.model_mapping as Record<string,string>).alias).toBe("fixture-model");
    expect(()=>reuseDraft(newDraft("gemini"),source)).toThrow("平台或类型");
  });
  it("applies conditional protocols, model passthrough, Bedrock authentication and native retry range",()=>{
    let draft=newDraft("zhipu","apikey");draft=withProtocolDefaults(setField(draft,"credentials.account_mode","coding"));draft.name="Fixture";draft.credentials.api_key="fixture";
    expect(buildPayload(draft).credentials.base_url).toBe((draft.credentials.api_base_urls as Record<string,string>).chat_completions);
    expect(draft.credentials.api_base_urls).not.toHaveProperty("responses");
    draft=newDraft("openai","apikey");draft.extra.openai_passthrough=true;
    expect(buildPayload(draft).credentials.model_mapping).toBeUndefined();
    draft=newDraft("anthropic","bedrock");draft.name="Fixture";Object.assign(draft.credentials,{auth_mode:"apikey",api_key:"fixture",aws_access_key_id:"old",aws_secret_access_key:"old",pool_mode:true,pool_mode_retry_count:0,aws_force_global:true});
    expect(validateDraft(draft)).toBe("");expect(buildPayload(draft).credentials).toMatchObject({api_key:"fixture",pool_mode_retry_count:0,aws_force_global:"true"});expect(buildPayload(draft).credentials.aws_secret_access_key).toBeUndefined();
    draft.credentials.pool_mode_retry_count=11;expect(validateDraft(draft)).toContain("范围");
  });
  it("rejects forbidden headers, invalid model rules and incomplete credentials",()=>{
    const draft=newDraft("openai","apikey");draft.name="Fixture";
    expect(validateDraft(draft)).toContain("API Key");draft.credentials.api_key="fixture";
    draft.credentials.header_override_enabled=true;draft.credentials.header_overrides={Authorization:"fixture"};expect(validateDraft(draft)).toContain("认证头");
    draft.credentials.header_overrides={"X-Fixture":"line\nbreak"};expect(validateDraft(draft)).toContain("请求头");
    draft.credentials.header_overrides={};draft.credentials.model_mapping={"bad*middle":"target"};expect(validateDraft(draft)).toContain("模型 ID");
    draft.credentials.model_mapping={"gpt-*":"gpt-*"};expect(validateDraft(draft)).toBe("");
    draft.credentials.temp_unschedulable_enabled=true;draft.credentials.temp_unschedulable_rules=[];expect(validateDraft(draft)).toContain("规则");
  });
  it("opens only supported authorization branches and preserves fresh native metadata",()=>{
    expect(authProvider(newDraft("anthropic","setup-token"))).toBe("setup-token");
    expect(authMethods(newDraft("anthropic")).map(m=>m.id)).toEqual(["manual","cookie"]);
    expect(authMethods(newDraft("openai")).map(m=>m.id)).toEqual(["manual","refresh","mobile-refresh","codex-session","agent-identity","codex-pat"]);
    expect(authMethods(newDraft("gemini")).map(m=>m.id)).toEqual(["manual"]);
    expect(authMethods(newDraft("grok")).map(m=>m.id)).toEqual(["manual","refresh","grok-sso"]);
    const gemini=newDraft("gemini");gemini.proxy_id=2;gemini.credentials.project_id="fixture";
    expect(authConfig(gemini)).toEqual({proxy_id:2,project_id:"fixture",oauth_type:"code_assist",tier_id:"gcp_standard"});
    expect(tokenIdentity("gemini",{access_token:"fixture",expires_at:100,extra:{drive_storage_limit:0,drive_storage_usage:0,password:"fixture-private"}},"fixture-rt")).toEqual({credentials:{access_token:"fixture",expires_at:"100",refresh_token:"fixture-rt"},extra:{drive_storage_limit:0,drive_storage_usage:0}});
    expect(tokenIdentity("grok",{access_token:"fixture",password:"private",sso:"private",sub:"fixture-user"}).credentials).toEqual({access_token:"fixture",sub:"fixture-user"});
    const payload=importParameters(newDraft("openai"));expect(payload).not.toHaveProperty("platform");expect(payload).not.toHaveProperty("type");expect(payload).toHaveProperty("credential_extras");
    expect(fields.filter(f=>fieldVisible(f,newDraft("gemini","service_account"))).map(f=>f.path)).not.toContain("credentials.project_id");
  });
});
