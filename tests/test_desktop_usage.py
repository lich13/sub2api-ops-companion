from __future__ import annotations

import json
import tempfile
import threading
import unittest
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import HTTPException
from app.desktop_api import DesktopService, UsageActionRequest, account_dto
from app.desktop_usage import attach_stats, project_usage, reset_credits, stats_specs

NOW = datetime(2026, 9, 25, 16, 5, tzinfo=timezone.utc)


def row(platform="openai", kind="oauth", **extra):
    return {"id": 1, "name": "usage", "platform": platform, "type": kind, "status": "active",
            "schedulable": True, "updated_at": NOW, "quota_plan_type": "plus", "extra": extra}


class UsageProjectionTests(unittest.TestCase):
    def test_openai_window_boundary_stats_and_estimate(self):
        account = row(codex_5h_used_percent=25, codex_7d_used_percent=99,
                      codex_5h_reset_at=(NOW+timedelta(hours=2)).isoformat(),
                      codex_usage_updated_at=NOW.isoformat())
        usage = project_usage(account, NOW)
        specs = stats_specs(1, usage)
        self.assertEqual(specs[0]["start_at"], (NOW-timedelta(hours=3)).isoformat())
        self.assertEqual(specs[1]["start_at"], (NOW-timedelta(days=7)).isoformat())
        attach_stats(1, usage, {(1, "codex_7d"): {"cost": 76.8}}, free_token_limit=500_000)
        self.assertAlmostEqual(usage["windows"][1]["estimated_total_cost"], 76.8/0.99)
        self.assertNotIn("stats_start", usage["windows"][0])

    def test_grok_paid_uses_bill_not_request_headers(self):
        account = row("grok", subscription_tier="supergrok", grok_usage_snapshot={"requests": {"limit": 10, "remaining": 1}},
                      grok_billing_snapshot={"period_type":"weekly", "usage_percent":100, "used_cents":50,
                        "monthly_limit_cents":100, "prepaid_balance":1.5, "period_start":(NOW-timedelta(days=2)).isoformat(),
                        "period_end":(NOW+timedelta(days=5)).isoformat(), "fetched_at":NOW.isoformat(),
                        "partial":True, "failed_windows":["monthly"], "monthly_status_code":502})
        usage = project_usage(account, NOW)
        self.assertEqual(usage["branch"], "grok_paid")
        self.assertEqual([(w["label"], w["used_percent"], w["status"]) for w in usage["windows"]], [("7d",100,"known"),("30d",50,"error")])
        self.assertEqual(usage["prepaid_balance"],1.5)
        self.assertEqual(usage["actions"],["probe_quota"])
        self.assertNotIn("requests", json.dumps(usage))
        self.assertEqual(project_usage(account,NOW+timedelta(hours=2))["windows"][0]["status"],"stale")

    def test_grok_free_rolling_tokens_missing_and_zero(self):
        usage=project_usage(row("grok",subscription_tier="free"),NOW)
        self.assertEqual(stats_specs(1,usage)[0]["start_at"],(NOW-timedelta(hours=24)).isoformat())
        attach_stats(1,usage,{},free_token_limit=500_000)
        self.assertEqual(usage["windows"][0]["status"],"unknown")
        attach_stats(1,usage,{(1,"grok_24h"):{"tokens":250_000}},free_token_limit=500_000)
        self.assertEqual(usage["windows"][0]["used_percent"],50)
        attach_stats(1,usage,{(1,"grok_24h"):{"tokens":0}},free_token_limit=500_000)
        self.assertEqual(usage["windows"][0]["used_percent"],0)

    def test_grok_credentials_tier_overrides_stale_snapshot(self):
        account = row("grok", subscription_tier="free", grok_billing_snapshot={"plan":"free"})
        account["quota_grok_tier"] = "supergrok"
        self.assertEqual(project_usage(account, NOW)["branch"], "grok_paid")
        account["quota_grok_tier"] = "free"
        account["extra"]["grok_billing_snapshot"] = {"usage_percent":40}
        self.assertEqual(project_usage(account, NOW)["branch"], "grok_free")

    def test_key_today_midnight_and_only_configured_quotas(self):
        for platform in ("openai","grok"):
            usage=project_usage(row(platform,"apikey",quota_daily_limit=10,quota_daily_used=2.5,
                quota_weekly_limit=20,quota_weekly_used=18,quota_limit=100,quota_used=90,
                quota_daily_start=NOW.isoformat(),quota_weekly_reset_mode="fixed",
                quota_weekly_reset_at=(NOW+timedelta(days=1)).isoformat()),NOW)
            self.assertEqual(usage["today_start"],"2026-09-26T00:00:00+08:00")
            self.assertEqual([w["used_percent"] for w in usage["windows"]],[25,90,90])
            self.assertEqual(usage["windows"][0]["reset_at"],(NOW+timedelta(days=1)).isoformat())
            self.assertEqual(usage["actions"],[])
            self.assertEqual(project_usage(row(platform,"apikey"),NOW)["windows"],[])

    def test_no_credentials_points_or_invites_and_expired_credit_filtered(self):
        extra={"codex_reset_credit_snapshot":{"available_count":2,"credits":[
                {"expires_at":(NOW-timedelta(seconds=1)).isoformat()},
                {"expires_at":(NOW+timedelta(days=1)).isoformat(),"id":"hidden-credit"}]},
                "codex_credits_snapshot":{"balance":123},"referral":"hidden-referral"}
        usage=project_usage({**row(**extra),"parent_account_id":2},NOW)
        self.assertEqual(usage["reset_credits"]["available"],1)
        self.assertNotIn("reset_quota",usage["actions"])
        self.assertNotIn("hidden",json.dumps(usage,default=str))
        self.assertNotIn("123",json.dumps(usage,default=str))
        self.assertIsNone(reset_credits({},NOW)["available"])


class UsageActionTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory()
        self.calls=[]; self.block=None; self.entered=threading.Event(); self.result={}
        self.live=row(codex_reset_credit_snapshot={"available_count":1})
        owner=self
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self): self.respond()
            def do_POST(self): self.respond()
            def respond(self):
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                owner.calls.append((self.command,self.path,self.headers.get("x-api-key")))
                owner.entered.set()
                if owner.block: owner.block.wait(2)
                if self.path.endswith("reset-quota"):
                    owner.live["extra"]["codex_reset_credit_snapshot"]["available_count"]=0
                self.send_response(200);self.send_header("Content-Type","application/json");self.end_headers()
                self.wfile.write(json.dumps({"code":0,"data":{"code":"ok","windows_reset":1,"cache_refreshed":True,"cache_persisted":True,
                    "credentials":{"api_key":"secret-marker"},"points":99,"referral":"private-invite",**owner.result}}).encode())
            def log_message(self,*_): pass
        self.server=ThreadingHTTPServer(("127.0.0.1",0),Handler)
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        db=Mock();db.fetch_one.side_effect=lambda *_a,**_k:self.live
        self.service=DesktopService(SimpleNamespace(db=db,oauth_base_url=lambda:f"http://127.0.0.1:{self.server.server_port}",
            settings=SimpleNamespace(audit_path=str(Path(self.tmp.name)/"audit.jsonl"))))
        self.service.snapshot=Mock(side_effect=lambda:{"accounts":[{"id":1,"usage":project_usage(self.live,NOW)}]})

    def tearDown(self):
        self.server.shutdown();self.server.server_close();self.thread.join();self.tmp.cleanup()

    def payload(self,action,**kwargs):
        return UsageActionRequest(action=action,expected_version=account_dto(self.live,NOW,set())["version"],**kwargs)

    def test_exact_upstream_paths_methods_and_whitelist(self):
        for action in ("query_usage","query_reset_credits","reset_quota"):
            result=self.service.usage_action(1,self.payload(action,confirmed=True),"capture-key")
            self.assertNotIn("secret-marker",json.dumps(result,default=str))
            self.assertNotIn("private-invite",json.dumps(result,default=str))
        self.live=row("grok")
        self.service.usage_action(1,self.payload("probe_quota",confirmed=True),"capture-key")
        self.assertEqual([(m,p) for m,p,_ in self.calls],[
            ("GET","/api/v1/admin/accounts/1/usage?source=active&force=true"),
            ("POST","/api/v1/admin/openai/accounts/1/quota/refresh"),
            ("POST","/api/v1/admin/openai/accounts/1/reset-quota"),
            ("GET","/api/v1/admin/grok/accounts/1/quota")])
        self.assertTrue(all(key=="capture-key" for _,_,key in self.calls))
        self.assertNotIn("capture-key",Path(self.tmp.name,"audit.jsonl").read_text())

    def test_confirmation_version_type_deleted_and_credit_checks(self):
        tests=[("reset_quota",False), ("probe_quota",False)]
        for action,confirmed in tests:
            with self.assertRaises(HTTPException):self.service.usage_action(1,self.payload(action,confirmed=confirmed),"key")
        payload=self.payload("query_usage");self.live["updated_at"]=NOW+timedelta(seconds=1)
        with self.assertRaises(HTTPException):self.service.usage_action(1,payload,"key")
        self.live=row(kind="apikey")
        with self.assertRaises(HTTPException):self.service.usage_action(1,self.payload("query_usage"),"key")
        self.live=None
        with self.assertRaises(HTTPException):self.service.usage_action(1,payload,"key")
        self.assertEqual(self.calls,[])

    def test_serial_execution_rejects_double_click(self):
        self.block=threading.Event(); payload=self.payload("query_usage")
        worker=threading.Thread(target=lambda:self.service.usage_action(1,payload,"key"));worker.start()
        self.assertTrue(self.entered.wait(1))
        with self.assertRaises(HTTPException) as raised:self.service.usage_action(1,payload,"key")
        self.assertEqual(raised.exception.status_code,409)
        self.block.set();worker.join();self.assertEqual(len(self.calls),1)

    def test_uncertain_reset_never_replays_until_count_query(self):
        payload=self.payload("reset_quota",confirmed=True)
        with patch("app.desktop_api._urlopen_no_redirect",side_effect=TimeoutError) as send:
            with self.assertRaises(HTTPException):self.service.usage_action(1,payload,"key")
            with self.assertRaises(HTTPException):self.service.usage_action(1,payload,"key")
            self.assertEqual(send.call_count,1)
            self.assertEqual(send.call_args.kwargs["timeout"],90)
        self.service.usage_action(1,self.payload("query_reset_credits"),"key")
        self.assertNotIn(1,self.service._uncertain_resets)

    def test_monitor_busy_rejects_openai_but_not_grok(self):
        lock=threading.Lock();lock.acquire();self.service.r.oauth_monitor=SimpleNamespace(_run_lock=lock)
        with self.assertRaises(HTTPException):self.service.usage_action(1,self.payload("query_usage"),"key")
        self.live=row("grok")
        self.service.usage_action(1,self.payload("probe_quota",confirmed=True),"key")
        self.assertEqual(len(self.calls),1);self.assertTrue(lock.locked());lock.release()

    def test_partial_probe_and_unpersisted_count_are_not_success(self):
        self.live=row("grok")
        self.result={"probe_error":"private upstream body", "status_code":429}
        with self.assertRaises(HTTPException) as raised:
            self.service.usage_action(1,self.payload("probe_quota",confirmed=True),"key")
        self.assertEqual(raised.exception.detail["code"],"probe_failed")
        self.assertNotIn("private",json.dumps(raised.exception.detail))
        self.live=row(); self.result={"cache_persisted":False}
        self.service._uncertain_resets.add(1)
        with self.assertRaises(HTTPException):
            self.service.usage_action(1,self.payload("query_reset_credits"),"key")
        self.assertIn(1,self.service._uncertain_resets)

    def test_reset_cache_warning_keeps_uncertain_guard(self):
        self.result={"cache_refreshed":False,"warning_code":"cache_failed"}
        result=self.service.usage_action(1,self.payload("reset_quota",confirmed=True),"key")
        self.assertIn("勿重复重置",result["message"])
        self.assertIn(1,self.service._uncertain_resets)


class ReadOnlySnapshotTests(unittest.TestCase):
    def test_repeated_refresh_reads_saved_evidence_and_shared_stats_only(self):
        db=Mock()
        account=row(kind="apikey",quota_limit=10,quota_used=1)
        def fetch(sql,*_):
            if "jsonb_to_recordset" in sql:
                return [{"account_id":1,"key":"today","requests":3,"tokens":12,
                         "cost":2,"standard_cost":3,"user_cost":4}]
            if "FROM accounts a" in sql:
                return [account]
            return []
        db.fetch_all.side_effect=fetch
        runtime=SimpleNamespace(db=db,key_fallback_controller=None,oauth_monitor=None,
            build_model_guard_panel=lambda:{"incidents":[]})
        service=DesktopService(runtime)
        with patch("app.desktop_api._urlopen_no_redirect",side_effect=AssertionError("read refresh sent a request")):
            first=service.snapshot()
            service._cached_at=0
            second=service.snapshot()
        self.assertEqual(first["accounts"][0]["usage"]["today"]["tokens"],12)
        self.assertEqual(second["accounts"][0]["usage"]["today"],first["accounts"][0]["usage"]["today"])
        self.assertEqual(sum("jsonb_to_recordset" in call.args[0] for call in db.fetch_all.call_args_list),1)
