from __future__ import annotations

import asyncio
import json
import os
import tempfile
import threading
import unittest
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

os.environ.setdefault("OPS_SESSION_SECRET", "desktop-test-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from fastapi import HTTPException
from app.desktop_api import DesktopService, ScheduleRequest, account_dto, error_dto, safe_error_body, group_dtos
from app.config_service import ConfigConflict
from app.key_fallback import KeyFallbackController
from app import main as main_module
from app.settings import Settings

NOW = datetime.now(timezone.utc)


def row(**changes):
    return {"id": 12, "name": "测试账号", "platform": "openai", "type": "apikey", "status": "active",
            "schedulable": False, "updated_at": NOW, "group_ids": [1, 2], "credentials": {"token": "never-return-me"}, **changes}


class DesktopEvidenceTests(unittest.TestCase):
    def test_recent_accounts_are_distinct_and_group_scoped(self):
        def call(group, account, log):
            return {"id":group,"name":str(group),"platform":"openai","log_id":log,
                    "account_id":account,"account_name":str(account),"model":"gpt-6-sol","called_at":NOW}
        result = group_dtos([call(1, 3, 10), call(1, 2, 9), call(1, 2, 8), call(1, 1, 7), call(2, 1, 11)])
        self.assertEqual([a["account_id"] for a in result[0]["recent_accounts"]], [3, 2, 1])
        self.assertEqual(result[0]["account_id"], 3)
        self.assertEqual(result[1]["recent_accounts"][0]["log_id"], 11)

    def test_account_dto_never_returns_credentials(self):
        result = account_dto(row(), NOW, {12})
        self.assertNotIn("credentials", result)
        self.assertNotIn("never-return-me", json.dumps(result, default=str))
        self.assertTrue(result["managed"])
        self.assertEqual(result["group_ids"], [1, 2])

    def test_switch_on_is_not_availability(self):
        result = account_dto(row(schedulable=True, rate_limit_reset_at=NOW+timedelta(minutes=5)), NOW, set())
        self.assertTrue(result["schedulable"])
        self.assertFalse(result["available"])
        self.assertEqual(result["blockers"][0]["label"], "限流中")

    def test_expired_blockers_and_success_after_error(self):
        result = account_dto(row(schedulable=True, rate_limit_reset_at=NOW-timedelta(seconds=5),
            last_success_at=NOW, last_error_at=NOW-timedelta(minutes=1)), NOW, set())
        self.assertTrue(result["available"])
        self.assertTrue(result["success_after_error"])

    def test_new_error_is_not_hidden_by_older_success(self):
        result = account_dto(row(last_success_at=NOW-timedelta(minutes=1), last_error_at=NOW), NOW, set())
        self.assertFalse(result["success_after_error"])

    def test_no_error_time_is_invented(self):
        result = account_dto(row(error_message="re-auth required"), NOW, set())
        self.assertIsNone(result["last_error_at"])
        self.assertEqual(result["error_message"], "re-auth required")

    def test_error_body_excludes_request_and_credentials(self):
        value = {"error": {"code": "invalid_token", "message": "Bearer secretsecret", "prompt": "private-prompt", "request": {"input":"private-input"}},
                 "request_body": "private-body", "authorization": "private-auth"}
        text = safe_error_body(json.dumps(value))
        self.assertIn("invalid_token", text)
        for secret in ("secretsecret", "private-prompt", "private-input", "private-body", "private-auth"):
            self.assertNotIn(secret, text)

    def test_error_detail_is_bounded_and_does_not_return_raw_columns(self):
        result = error_dto({"error_body": json.dumps({"error":{"message":"x"*20000}}), "client_ip":"private-ip", "error_message":"api_key=privatekey"}, True)
        self.assertLessEqual(len(result["content"].encode()),6000)
        self.assertNotIn("client_ip",result)
        self.assertNotIn("privatekey",result["message"])

    def test_empty_error_body(self):
        self.assertEqual(safe_error_body('{"prompt":"hidden"}'), "")

    def test_stringified_request_dump_is_hidden_in_detail_and_summary(self):
        dump=json.dumps({'request':{'body':{'messages':[{'role':'user','content':'PRIVATE_PROMPT_MARKER'}]},'credentials':{'openai':'OPAQUE_CREDENTIAL_MARKER'}}})
        for value in (dump,'Request failed: '+dump,dump.replace('"','\\"')):
            result=error_dto({'error_message':value,'error_body':{'error':{'message':value}}},True)
            self.assertNotIn('PRIVATE_PROMPT_MARKER',json.dumps(result))
            self.assertNotIn('OPAQUE_CREDENTIAL_MARKER',json.dumps(result))

    def test_errors_use_account_filter_and_cursor(self):
        db=Mock();db.fetch_all.return_value=[{"id":9},{"id":8},{"id":7}]
        service=DesktopService(SimpleNamespace(db=db))
        result=service.errors(12,10,2)
        self.assertEqual(result["next_cursor"],8)
        self.assertEqual([r["id"] for r in result["items"]],[9,8])
        sql,params=db.fetch_all.call_args.args
        self.assertEqual(params,{"account_id":12,"before_id":10,"limit":3})
        self.assertIn("e.account_id",sql)


class DesktopAuthTests(unittest.TestCase):
    def setUp(self):
        self.calls=[]
        calls=self.calls
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                calls.append((self.path,self.headers.get("x-api-key")))
                success=self.headers.get("x-api-key")=="test-admin-key"
                self.send_response(200 if success else 401);self.send_header("Content-Type","application/json");self.end_headers()
                self.wfile.write(json.dumps({"code":0,"data":[]} if success else {"code":401}).encode())
            def log_message(self,*_): pass
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        self.service=DesktopService(SimpleNamespace(oauth_base_url=lambda:f'http://127.0.0.1:{self.server.server_port}'))
    def tearDown(self):
        self.server.shutdown();self.server.server_close();self.thread.join()
    def test_cached_reads_fresh_writes_and_no_key_in_url(self):
        self.service.authenticate('test-admin-key');self.service.authenticate('test-admin-key')
        self.assertEqual(len(self.calls),1)
        self.service.authenticate('test-admin-key',fresh=True)
        self.assertEqual(len(self.calls),2)
        self.assertTrue(all(path=='/api/v1/admin/groups/all' for path,_ in self.calls))
        self.assertNotIn('test-admin-key',str(self.service._auth))
    def test_bad_key_and_newline_rejected(self):
        for key in ('','bad','bad\nkey'):
            with self.assertRaises(HTTPException) as raised:self.service.authenticate(key)
            self.assertEqual(raised.exception.status_code,401)


class DesktopScheduleTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory()
        settings=SimpleNamespace(key_fallback_config_path=str(Path(self.tmp.name)/'key.json'),audit_path=str(Path(self.tmp.name)/'audit.jsonl'))
        self.live=row()
        self.db=Mock();self.db.fetch_one.side_effect=lambda *_args,**_kwargs:dict(self.live) if self.live else None
        self.controller=KeyFallbackController(settings,self.db,base_url_provider=lambda:'http://localhost',admin_token_provider=lambda:'x',key_inventory=lambda _:[row()])
        self.controller.save_user_config(openai_enabled=False,grok_enabled=True,managed_account_ids=[12],user='test')
        self.service=DesktopService(SimpleNamespace(db=self.db,settings=settings,key_fallback_controller=self.controller,oauth_base_url=lambda:'http://localhost'))
        self.payload=ScheduleRequest(schedulable=True,expected_version=account_dto(self.live,NOW,{12})['version'],detach_managed=True)
    def tearDown(self):self.tmp.cleanup()
    def test_detach_precedes_only_schedulable_request_and_preserves_switches(self):
        def write(account_id,enabled,**_kwargs):
            self.assertNotIn(12,self.controller.load_config().managed_account_ids)
            self.assertTrue(self.controller._lock._is_owned())
            self.live['schedulable']=enabled
            return {'success':True}
        with patch('app.desktop_api.execute_sub2api_set_schedulable',side_effect=write) as send:
            result=self.service.set_schedulable(12,self.payload,'test-key')
        self.assertTrue(result['verified']);self.assertTrue(result['detached'])
        self.assertTrue(self.controller.load_config().grok_enabled)
        self.assertFalse(self.controller.load_config().openai_enabled)
        self.assertEqual(send.call_args.args,(12,True))
    def test_failed_detach_aborts_write(self):
        with patch.object(self.controller,'_write_config_unlocked',side_effect=OSError('disk full')),patch('app.desktop_api.execute_sub2api_set_schedulable') as send:
            with self.assertRaises(OSError):self.service.set_schedulable(12,self.payload,'key')
            send.assert_not_called()
    def test_failed_api_does_not_reenroll(self):
        with patch('app.desktop_api.execute_sub2api_set_schedulable',return_value={'success':False,'error_code':'timeout'}):
            with self.assertRaises(HTTPException) as raised:self.service.set_schedulable(12,self.payload,'key')
        self.assertTrue(raised.exception.detail['detached'])
        self.assertNotIn(12,self.controller.load_config().managed_account_ids)
    def test_changed_or_deleted_account_aborts_before_detach(self):
        for live in (row(type='oauth'),{}):
            self.live=live
            with patch('app.desktop_api.execute_sub2api_set_schedulable') as send:
                with self.assertRaises(HTTPException):self.service.set_schedulable(12,self.payload,'key')
                send.assert_not_called()
            self.assertIn(12,self.controller.load_config().managed_account_ids)
    def test_managed_requires_confirmation(self):
        self.payload.detach_managed=False
        with self.assertRaises(HTTPException) as raised:self.service.set_schedulable(12,self.payload,'key')
        self.assertEqual(raised.exception.status_code,409)
    def test_readback_required(self):
        with patch('app.desktop_api.execute_sub2api_set_schedulable',return_value={'success':True}):
            with self.assertRaises(HTTPException) as raised:self.service.set_schedulable(12,self.payload,'key')
        self.assertEqual(raised.exception.detail['code'],'readback_mismatch')


class DesktopConfigTests(unittest.IsolatedAsyncioTestCase):
    async def test_partial_save_and_stale_revision(self):
        from app.config_service import ConfigService
        with tempfile.TemporaryDirectory() as directory:
            s=Settings(database_url='unused',session_secret='test',session_ttl_seconds=3600,base_path='',audit_path=str(Path(directory)/'audit'),oauth_config_path=str(Path(directory)/'oauth.json'))
            with patch.object(main_module,'settings',s),patch.object(main_module,'oauth_monitor',None):
                main_module.save_oauth_runtime_config({'oauth_recovery_test_model_id':'preserve-model','oauth_daily_test_time':'05:00'})
                service=ConfigService(main_module)
                old=service.snapshot('oauth')['oauth']['revision']
                await service.save('oauth',{'oauth_daily_test_time':'06:15'},'test',old)
                data=json.loads(Path(s.oauth_config_path).read_text())
                self.assertEqual(data['oauth_recovery_test_model_id'],'gpt-5.6-luna')
                self.assertEqual(data['oauth_daily_test_time'],'06:15')
                self.assertEqual(Path(s.oauth_config_path).stat().st_mode&0o777,0o600)
                with self.assertRaises(ConfigConflict):await service.save('oauth',{'oauth_daily_test_time':'07:00'},'test',old)
                with self.assertRaises(ValueError):await service.save('oauth',{'oauth_daily_test_time':'25:00'},'test')
                with self.assertRaises(ValueError):await service.save('oauth',{'oauth_usage_refresh_enabled':True},'test')
                with self.assertRaises(ValueError):await service.save('oauth',{'oauth_regular_refresh_interval_seconds':60},'test')
                self.assertEqual(json.loads(Path(s.oauth_config_path).read_text())['oauth_daily_test_time'],'06:15')


if __name__ == '__main__':unittest.main()
