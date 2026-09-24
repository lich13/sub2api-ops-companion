"""Opt-in integration runner for a disposable Sub2API/database Docker network.

Never run against production. Requires the exact isolated hostname and an empty
account table. Only the local mock below can receive inference requests.
"""
from __future__ import annotations

import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

import psycopg
from psycopg.types.json import Jsonb

DB = os.environ["DATABASE_URL"]
if os.environ.get("DESKTOP_QA_DISPOSABLE") != "sub2ops-desktop" or urlparse(DB).hostname != "pg":
    raise SystemExit("Refusing: dedicated disposable QA database required")
BASE = "http://sub2api:8080"
OPS = "http://127.0.0.1:18081/api/desktop/v1"
KEY = "desktop-isolated-test-key"


def request(url, method="GET", body=None, headers=None):
    req = urllib.request.Request(url, data=json.dumps(body).encode() if body is not None else None,
                                 headers={"Content-Type": "application/json", **(headers or {})}, method=method)
    try:
        with urllib.request.urlopen(req, timeout=15) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def ops(path, method="GET", body=None, key=KEY):
    return request(OPS+path, method, body, {"x-api-key": key})


def seed():
    with psycopg.connect(DB) as db:
        assert db.execute("SELECT count(*) FROM accounts").fetchone()[0] == 0, "QA must start empty"
        status, login = request(BASE+"/api/v1/auth/login", "POST", {
            "email": "desktop-qa@example.invalid", "password": "Desktop-QA-only-9264"})
        assert status == 200, (status, login)
        token = login["data"]["access_token"]
        status, generated = request(BASE+"/api/v1/admin/settings/admin-api-key/regenerate", "POST", {},
                                    {"Authorization": "Bearer "+token})
        assert status == 200, status
        # Replace only the newly generated disposable credential, never log it.
        actual = generated["data"].get("key") or generated["data"].get("api_key")
        assert actual
        assert db.execute("UPDATE settings SET value=%s WHERE value=%s", (KEY, actual)).rowcount == 1
        user = db.execute("SELECT id FROM users WHERE email='desktop-qa@example.invalid'").fetchone()[0]
        db.execute("UPDATE users SET balance=100 WHERE id=%s", (user,))
        groups = [db.execute("INSERT INTO groups(name,platform) VALUES (%s,'openai') RETURNING id", (name,)).fetchone()[0]
                  for name in ("QA Alpha", "QA Beta")]
        accounts = [db.execute("INSERT INTO accounts(name,platform,type,credentials) VALUES (%s,'openai','apikey',%s) RETURNING id",
                    (name, Jsonb({"base_url":"http://127.0.0.1:18082", "api_key":"qa-upstream-only", "model_mapping":{"gpt-5.6-sol":"gpt-5.6-sol"}}))).fetchone()[0]
                    for name in ("QA Shared", "QA Secondary")]
        for account, group in ((accounts[0],groups[0]),(accounts[0],groups[1]),(accounts[1],groups[1])):
            db.execute("INSERT INTO account_groups(account_id,group_id) VALUES (%s,%s)",(account,group))
        api = db.execute("INSERT INTO api_keys(user_id,key,name,group_id) VALUES (%s,'sk-qa-isolated','QA Gateway',%s) RETURNING id",(user,groups[0])).fetchone()[0]
        for account, group, ago in ((accounts[0],groups[0],30),(accounts[0],groups[1],20),(accounts[1],groups[1],10)):
            db.execute("INSERT INTO usage_logs(user_id,api_key_id,account_id,group_id,model,created_at) VALUES (%s,%s,%s,%s,'gpt-5.6-sol',now()-(%s * interval '1 second'))",(user,api,account,group,ago))
        error = db.execute("""INSERT INTO ops_error_logs(account_id,group_id,platform,model,error_phase,error_owner,error_type,
                   status_code,upstream_status_code,error_message,upstream_error_detail,created_at,request_id)
                   VALUES (%s,%s,'openai','gpt-5.6-sol','upstream','provider','upstream_error',502,502,
                   'qa upstream rejected',%s,now()-interval '1 minute','desktop-qa-request') RETURNING id""",
                   (accounts[0],groups[0],json.dumps({"error":{"code":"qa_error","message":"QA error"},"request":{"prompt":"PRIVATE_QA_PROMPT"},"credentials":{"api_key":"PRIVATE_QA_KEY"}}))).fetchone()[0]
    print(json.dumps({"seeded":True,"accounts":accounts,"groups":groups,"error_id":error}))


def verify():
    status,snapshot=ops('/snapshot'); assert status==200,(status,snapshot)
    accounts=snapshot['accounts']; groups=snapshot['groups']
    shared=next(a for a in accounts if a['name']=='QA Shared')
    other=next(a for a in accounts if a['name']=='QA Secondary')
    assert len(shared['group_ids'])==2
    assert next(g for g in groups if g['name']=='QA Alpha')['account_id']==shared['id']
    assert next(g for g in groups if g['name']=='QA Beta')['account_id']==other['id']
    assert shared['success_after_error']
    assert not any(k in json.dumps(snapshot) for k in ('credentials','qa-upstream-only','PRIVATE_QA_KEY','PRIVATE_QA_PROMPT'))
    _,detail=ops('/errors/'+str(shared['last_error_id']))
    assert 'qa_error' in detail['content'] and 'PRIVATE_QA_' not in json.dumps(detail)
    assert ops('/snapshot',key='invalid')[0]==401
    _,config=ops('/config'); old=config['oauth']['revision']
    status,_=ops('/config/oauth','PUT',{'expected_revision':old,'changes':{'oauth_daily_test_time':'06:15'}});assert status==200
    assert ops('/config/oauth','PUT',{'expected_revision':old,'changes':{'oauth_daily_test_time':'07:15'}})[0]==409
    fallback=config['key_fallback']
    assert ops('/config/key_fallback','PUT',{'expected_revision':fallback['revision'],'changes':{'managed_account_ids':[shared['id'],other['id']]}})[0]==200
    for enabled in (False,True):
        _,snapshot=ops('/snapshot'); live=next(a for a in snapshot['accounts'] if a['id']==shared['id'])
        status,result=ops(f"/accounts/{shared['id']}/schedulable",'POST',{'schedulable':enabled,'expected_version':live['version'],'detach_managed':True})
        assert status==200 and result['verified'],(status,result)
    _,config=ops('/config')
    assert config['key_fallback']['managed_account_ids']==[other['id']]
    with psycopg.connect(DB) as db:
        state=db.execute('SELECT schedulable,status,credentials FROM accounts WHERE id=%s',(shared['id'],)).fetchone()
        assert state[0] and state[1]=='active' and state[2]['api_key']=='qa-upstream-only'
    print(json.dumps({'desktop_integration':'passed','real_admin_auth':True,'real_schedulable_api':True,'group_evidence':True,'redaction':True,'revision_conflict':True,'managed_detach':True}))


class MockUpstream(BaseHTTPRequestHandler):
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))
        if 'QA_FORCE_ERROR' in json.dumps(body):
            self.send_response(500);self.send_header('Content-Type','application/json');self.end_headers()
            self.wfile.write(json.dumps({'error':{'code':'qa_synthetic_error','message':'Isolated upstream rejected this request'},'request':{'prompt':'PRIVATE_QA_PROMPT'}}).encode());return
        result={'id':'chatcmpl-qa','object':'chat.completion','created':int(time.time()),'model':body.get('model','gpt-5.6-sol'),
                'choices':[{'index':0,'message':{'role':'assistant','content':'QA success'},'finish_reason':'stop'}],
                'usage':{'prompt_tokens':2,'completion_tokens':2,'total_tokens':4}}
        if self.path.endswith('/responses'):
            result={'id':'resp-qa','object':'response','created_at':int(time.time()),'status':'completed','model':body.get('model'),
                    'output':[{'id':'msg-qa','type':'message','status':'completed','role':'assistant','content':[{'type':'output_text','text':'QA success','annotations':[]}]}],
                    'usage':{'input_tokens':2,'output_tokens':2,'total_tokens':4}}
        self.send_response(200)
        if body.get('stream') and self.path.endswith('/responses'):
            self.send_header('Content-Type','text/event-stream');self.end_headers()
            self.wfile.write(('event: response.completed\ndata: '+json.dumps({'type':'response.completed','response':result})+'\n\n').encode())
        else:
            self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps(result).encode())
    def log_message(self,*_): pass


def gateway():
    _,before=ops('/snapshot')
    previous=next(a for a in before['accounts'] if a['name']=='QA Shared')['last_error_id'] or 0
    for content,expected in (('QA success',200),('QA_FORCE_ERROR',500),('QA recovered',200)):
        status,body=request(BASE+'/v1/chat/completions','POST',{'model':'gpt-5.6-sol','messages':[{'role':'user','content':content}],'stream':False},
                            {'Authorization':'Bearer sk-qa-isolated'})
        assert status==expected or (expected==500 and status==502),(status,body)
    for _ in range(10):
        _,snapshot=ops('/snapshot'); account=next(a for a in snapshot['accounts'] if a['name']=='QA Shared')
        if account['success_after_error'] and account['last_error_id']>previous:break
        time.sleep(.5)
    assert account['success_after_error'] and account['last_error_id']>previous
    _,detail=ops('/errors/'+str(account['last_error_id']))
    assert detail['upstream_status_code']==500 and 'PRIVATE_QA_PROMPT' not in json.dumps(detail)
    print(json.dumps({'mock_gateway':'passed','success_error_success':True,'error_id':account['last_error_id'],'success_after_error':True}))


if __name__=='__main__':
    {'seed':seed,'verify':verify,'gateway':gateway,'mock':lambda:ThreadingHTTPServer(('0.0.0.0',18082),MockUpstream).serve_forever()}[sys.argv[1]]()
