from __future__ import annotations

import copy
import json
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.desktop_api import DesktopService, ScheduleRequest, account_dto, install_desktop_api
from app.key_fallback import KeyFallbackController


class ManagementTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.now = datetime.now(timezone.utc)
        self.rows = {i: {"id": i, "name": f"Account {i}", "platform": "openai", "type": "apikey",
            "status": "active", "schedulable": False, "updated_at": self.now, "extra": {}, "priority": 3}
            for i in (1, 2, 3)}
        self.calls, self.behavior = [], "ok"
        self.entered, self.release = threading.Event(), threading.Event()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def respond(self, status, body):
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(body).encode())

            def do_GET(self):
                self.respond(200 if self.headers.get("x-api-key") == "qa-admin" else 401, {"code": 0, "data": []})

            def mutate(self):
                length = int(self.headers.get("Content-Length", 0))
                fixture.calls.append((self.command, self.path, self.rfile.read(length)))
                account_id = int(self.path.split("/")[5])
                if fixture.behavior == "hold_reject":
                    fixture.entered.set()
                    fixture.release.wait(2)
                    return self.respond(500, {"code": 500})
                if fixture.behavior == "reject":
                    return self.respond(500, {"code": 500})
                if self.command == "DELETE":
                    fixture.rows.pop(account_id, None)
                else:
                    fixture.rows[account_id].update(status="active", rate_limit_reset_at=None,
                        temp_unschedulable_until=None, overload_until=None, extra={})
                if fixture.behavior == "disconnect":
                    self.close_connection = True
                    return
                self.respond(200, {"code": 0, "data": {"credentials": "must-not-escape"}})

            do_DELETE = do_POST = mutate
            def log_message(self, *_): pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.01), daemon=True)
        self.thread.start()
        self.db = Mock()
        self.db.fetch_one.side_effect = lambda _sql, params: copy.deepcopy(self.rows.get(params["id"]))
        self.db.fetch_all.side_effect = lambda *_: [{"id": i, "name": row["name"]} for i, row in self.rows.items()]
        settings = SimpleNamespace(key_fallback_config_path=str(Path(self.tmp.name)/"key.json"),
            audit_path=str(Path(self.tmp.name)/"audit.jsonl"))
        base = lambda: f"http://127.0.0.1:{self.server.server_port}"
        self.controller = KeyFallbackController(settings, self.db, base_url_provider=base,
            admin_token_provider=lambda: "qa-admin", key_inventory=lambda _: list(self.rows.values()))
        self.controller.save_user_config(openai_enabled=False, grok_enabled=True, managed_account_ids=[1, 2], user="qa")
        self.store = Mock()
        self.store.cached_snapshot.return_value = {"recovery_history": {"old": {"id": 10, "account_id": 1}}}
        runtime = SimpleNamespace(db=self.db, settings=settings, key_fallback_controller=self.controller,
            oauth_base_url=base, oauth_monitor=SimpleNamespace(_run_lock=threading.Lock(), store=self.store))
        app = FastAPI()
        self.service = install_desktop_api(app, runtime)
        self.client = TestClient(app)

    def tearDown(self):
        self.client.close()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.tmp.cleanup()

    def request(self, account_id, action="delete", **changes):
        row = self.rows.get(account_id, {"id": account_id})
        payload = {"expected_version": account_dto(row, self.now, set())["version"]}
        payload.update(changes)
        return self.client.request("DELETE" if action == "delete" else "POST",
            f"/api/desktop/v1/accounts/{account_id}" + ("/recover-state" if action != "delete" else ""),
            headers={"x-api-key": "qa-admin"}, json=payload)

    def test_delete_detaches_only_target_and_reads_back(self):
        result = self.request(1, detach_managed=True)
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual(result.json(), {"account_id": 1, "deleted": True, "verified": True, "detached": True})
        self.assertEqual(self.calls, [("DELETE", "/api/v1/admin/accounts/1", b"")])
        config = self.controller.load_config()
        self.assertEqual(config.managed_account_ids, (2,))
        self.assertTrue(config.grok_enabled)
        self.assertFalse(config.openai_enabled)
        self.assertEqual(len(self.store.cached_snapshot()["recovery_history"]), 1)

    def test_parallel_deletes_do_not_conflict_with_each_other(self):
        with ThreadPoolExecutor(max_workers=3) as workers:
            responses = list(workers.map(lambda i: self.request(i, detach_managed=True), (1, 2, 3)))
        self.assertEqual([r.status_code for r in responses], [200, 200, 200])
        self.assertEqual(len(self.calls), 3)
        self.assertFalse(self.rows)
        self.assertEqual(self.controller.load_config().managed_account_ids, ())

    def test_same_account_overlap_is_rejected_not_queued_for_retry(self):
        self.behavior = "hold_reject"
        with ThreadPoolExecutor(max_workers=1) as worker:
            first = worker.submit(self.request, 3)
            try:
                self.assertTrue(self.entered.wait(1))
                self.assertEqual(self.request(3).status_code, 409)
            finally:
                self.release.set()
            self.assertEqual(first.result().status_code, 502)
        self.assertEqual(len(self.calls), 1)

    def test_bad_key_and_stale_version_never_write(self):
        result = self.client.request("DELETE", "/api/desktop/v1/accounts/1",
            headers={"x-api-key": "bad"}, json={"expected_version": "a" * 64})
        self.assertEqual(result.status_code, 401)
        self.assertEqual(self.request(1, expected_version="a" * 64).status_code, 409)
        self.assertEqual(self.request(99).status_code, 404)
        self.assertEqual(self.calls, [])

    def test_managed_confirmation_and_save_failure_abort(self):
        self.assertEqual(self.request(1).status_code, 409)
        with patch.object(self.controller, "_write_config_unlocked", side_effect=OSError("private-storage-error")):
            result = self.request(1, detach_managed=True)
        self.assertEqual(result.status_code, 503)
        self.assertNotIn("private-storage", result.text)
        self.assertEqual(self.calls, [])
        self.assertIn(1, self.controller.load_config().managed_account_ids)

    def test_delete_failure_keeps_detached_actual_result(self):
        self.behavior = "reject"
        result = self.request(1, detach_managed=True)
        self.assertEqual(result.status_code, 502)
        self.assertTrue(result.json()["detail"]["detached"])
        self.assertIn("已解除托管", result.text)
        self.assertNotIn(1, self.controller.load_config().managed_account_ids)
        self.assertIn(1, self.rows)
        self.assertEqual(len(self.calls), 1)

    def test_disconnected_delete_only_checks_actual_state(self):
        self.behavior = "disconnect"
        result = self.request(3)
        self.assertEqual(result.status_code, 200)
        self.assertTrue(result.json()["deleted"])
        self.assertEqual(len(self.calls), 1)

    def test_recover_exact_endpoint_no_schedule_or_test_history(self):
        self.rows[1].update(status="error", rate_limit_reset_at=self.now+timedelta(minutes=5))
        history = copy.deepcopy(self.store.cached_snapshot())
        result = self.request(1, "recover")
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual(self.calls, [("POST", "/api/v1/admin/accounts/1/recover-state", b"{}")])
        self.assertFalse(self.rows[1]["schedulable"])
        self.assertIn(1, self.controller.load_config().managed_account_ids)
        self.assertEqual(self.store.cached_snapshot(), history)
        self.assertNotIn("credentials", result.text)

    def test_recover_model_limit_and_expired_states(self):
        self.rows[3]["extra"] = {"model_rate_limits": {"model": {"rate_limit_reset_at": (self.now+timedelta(minutes=1)).isoformat()}}}
        self.assertTrue(account_dto(self.rows[3], self.now, set())["recoverable"])
        self.assertEqual(self.request(3, "recover").status_code, 200)
        self.rows[3]["rate_limit_reset_at"] = self.now-timedelta(seconds=1)
        self.assertFalse(account_dto(self.rows[3], self.now, set())["recoverable"])
        self.assertEqual(self.request(3, "recover").status_code, 409)
        self.assertEqual(len(self.calls), 1)

    def test_mutations_share_account_and_recovery_locks(self):
        lock = self.service.account_lock(1)
        lock.acquire()
        try:
            self.assertEqual(self.request(1, detach_managed=True).status_code, 409)
            self.assertEqual(self.request(1, "recover").status_code, 409)
            with self.assertRaises(HTTPException) as raised:
                self.service.set_schedulable(1, ScheduleRequest(schedulable=True,
                    expected_version=account_dto(self.rows[1], self.now, set())["version"]), "qa-admin")
            self.assertEqual(raised.exception.status_code, 409)
        finally:
            lock.release()
        monitor = self.service.r.oauth_monitor._run_lock
        monitor.acquire()
        try:
            self.assertEqual(self.request(3).status_code, 409)
        finally:
            monitor.release()
        self.assertEqual(self.calls, [])

    def test_recover_failure_and_disconnect_do_not_retry(self):
        self.rows[3]["status"] = "error"
        self.behavior = "reject"
        self.assertEqual(self.request(3, "recover").status_code, 502)
        self.behavior = "disconnect"
        self.assertEqual(self.request(3, "recover").status_code, 200)
        self.assertEqual(len(self.calls), 2)


class RecoveryPaginationTests(unittest.TestCase):
    def test_filters_before_cursor_without_changing_history(self):
        history = {str(i): {"id": i, "account_id": i, "account_name": f"Account {i}"} for i in range(1, 31)}
        db, store = Mock(), Mock()
        store.cached_snapshot.return_value = {"recovery_history": history}
        live = [{"id": i, "name": f"Account {i}"} for i in (1, 3, 8)]
        db.fetch_all.side_effect = lambda _: live
        runtime = SimpleNamespace(db=db, oauth_monitor=SimpleNamespace(store=store))
        service = DesktopService(runtime)
        first = service.recoveries(limit=2)
        self.assertEqual([r["id"] for r in first["items"]], [8, 3])
        self.assertEqual(first["next_cursor"], 3)
        self.assertEqual([r["id"] for r in service.recoveries(3, 2)["items"]], [1])
        live.pop()
        self.assertEqual([r["id"] for r in DesktopService(runtime).recoveries()["items"]], [3, 1])
        live.clear()
        self.assertEqual(service.recoveries(limit=2), {"items": [], "next_cursor": None})
        self.assertEqual(len(history), 30)
