from __future__ import annotations

import copy
import json
import re
import socket
import sqlite3
import tempfile
import threading
import unittest
from collections import deque
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from app.auto_reset import AutoResetController, execute_credit_request, quota_result, upstream_evidence
from app.oauth_monitor import OAuthMonitor, execute_sub2api_account_test
from app.oauth_queries import credential_fingerprint
from app.settings import Settings
from app.usage_query import execute_oauth_usage_query


NOW = datetime(2026, 9, 30, 8, tzinfo=timezone.utc)
ACCOUNT_ID = 7
ADMIN_TOKEN = "auto-reset-isolated-fixture-admin"


def wham(now, *, five=10, seven=20, observed=None):
    return {"fetched_at": (observed or now).isoformat(), "rate_limit": {
        "primary_window": {"limit_window_seconds": 18000, "used_percent": five,
                           "reset_at": int((now + timedelta(hours=5)).timestamp())},
        "secondary_window": {"limit_window_seconds": 604800, "used_percent": seven,
                             "reset_at": int((now + timedelta(days=5)).timestamp())}}}


class EvidenceDB:
    """Run evidence SQL with only PostgreSQL JSON and limiter syntax adapted."""

    ERROR_FIELDS = ("error_owner", "error_phase", "upstream_error_message", "error_message",
                    "error_body", "upstream_error_detail", "upstream_errors")

    def __init__(self):
        self.raw = sqlite3.connect(":memory:", check_same_thread=False)
        self.raw.row_factory = sqlite3.Row
        self.raw.executescript("""
            CREATE TABLE ops_error_logs (id INTEGER, account_id INTEGER, upstream_status_code INTEGER,
                                         status_code INTEGER, created_at TEXT, error_owner TEXT,
                                         error_phase TEXT, upstream_error_message TEXT, error_message TEXT,
                                         error_body TEXT, upstream_error_detail TEXT, upstream_errors TEXT);
            CREATE TABLE usage_logs (id INTEGER, account_id INTEGER, created_at TEXT);
        """)
        from app.error_evidence import is_local_throttle

        self.raw.create_function("fixture_local_throttle", len(self.ERROR_FIELDS),
                                 lambda *values: int(is_local_throttle(dict(zip(self.ERROR_FIELDS, values)))))
        self.reads = 0

    def event(self, record_id=1, *, at=None, upstream=429, status=429, account_id=ACCOUNT_ID, **fields):
        unknown = set(fields) - set(self.ERROR_FIELDS)
        if unknown:
            raise ValueError("Unsupported fixture error fields: " + ", ".join(sorted(unknown)))
        recorded = {"error_owner": "provider", "error_phase": "upstream", **fields}
        values = [json.dumps(recorded[name]) if isinstance(recorded.get(name), (dict, list))
                  else recorded.get(name) for name in self.ERROR_FIELDS]
        self.raw.execute("INSERT INTO ops_error_logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                         (record_id, account_id, upstream, status,
                          (at or NOW - timedelta(minutes=5)).isoformat(), *values))

    def fetch_one(self, sql, params=None):
        self.reads += 1
        if "to_jsonb(e)" in sql:
            from app.error_evidence import local_throttle_sql

            predicate = local_throttle_sql()
            if predicate not in sql:
                raise AssertionError("Structured evidence SQL must retain the local limiter exclusion")
            columns = ",".join("e." + name for name in self.ERROR_FIELDS)
            sql = sql.replace(predicate, "fixture_local_throttle(" + columns + ")")
            sql = re.sub(r"to_jsonb\(e\)->>?'([a-z_]+)'", r"e.\1", sql)
        sql = re.sub(r"%\((\w+)\)s", r":\1", sql)
        values = {key: value.isoformat() if isinstance(value, datetime) else value for key, value in (params or {}).items()}
        result = self.raw.execute(sql, values).fetchone()
        return dict(result) if result else None


class AutoResetFixture(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="auto-reset-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.db = EvidenceDB()
        self.addCleanup(self.db.raw.close)
        self.allowed_ports = set()
        self.external_requests = []
        connect = socket.socket.connect

        def isolated_connect(sock, address):
            if not isinstance(address, tuple) or address[0] != "127.0.0.1" or address[1] not in self.allowed_ports:
                self.external_requests.append(address)
                raise AssertionError("External network is forbidden in automatic reset tests")
            return connect(sock, address)

        guard = patch("socket.socket.connect", new=isolated_connect)
        guard.start()
        self.addCleanup(guard.stop)
        self.addCleanup(lambda: self.assertEqual(self.external_requests, []))
        self.case_number = 0
        self.fresh_case()

    def fresh_case(self):
        self.case_number += 1
        self.now = NOW
        self.version = 0
        self.calls = []
        self.request_results = deque()
        self.test_results = deque()
        self.request_hook = None
        self.schedule_hook = None
        self.test_hook = None
        self.recovery_hook = None
        self.query_credits = 1
        self.query_five, self.query_seven = 20, 100
        self.reset_five, self.reset_seven = 10, 20
        self.account_reads = 0
        self.db.raw.execute("DELETE FROM ops_error_logs")
        self.db.raw.execute("DELETE FROM usage_logs")
        self.db.event()
        observed = (NOW - timedelta(hours=2)).isoformat()
        self.row = {"id": ACCOUNT_ID, "name": "isolated-parent", "platform": "openai", "type": "oauth",
                    "status": "active", "schedulable": True, "concurrency": 1,
                    "updated_at": (NOW - timedelta(minutes=1)).isoformat(),
                    "credentials": {"plan_type": "plus", "access_token": "fixture-access-v1"},
                    "parent_account_id": None, "rate_limited_at": (NOW - timedelta(minutes=5)).isoformat(),
                    "rate_limit_reset_at": (NOW + timedelta(days=5)).isoformat(),
                    "temp_unschedulable_until": None, "temp_unschedulable_reason": "", "overload_until": None,
                    "extra": {"codex_usage_updated_at": observed, "codex_5h_used_percent": 20,
                              "codex_7d_used_percent": 100,
                              "codex_5h_reset_at": (NOW + timedelta(hours=5)).isoformat(),
                              "codex_7d_reset_at": (NOW + timedelta(days=5)).isoformat(),
                              "codex_reset_credit_snapshot": {"available_count": 1, "fetched_at": observed,
                                  "credits": [{"expires_at": (NOW + timedelta(days=2)).isoformat()}]}}}
        self.settings = Settings(database_url="postgresql://fixture:fixture@127.0.0.1/unused", base_path="",
            usage_query_state_path=str(self.root / f"state-{self.case_number}.json"),
            audit_path=str(self.root / "audit.jsonl"), oauth_daily_test_enabled=False,
            oauth_auto_reset_credit_enabled=True, oauth_recovery_test_model_id="fixture-recovery-model")
        self.base_url = "http://127.0.0.1:1"
        self.make_controller()

    def touch(self):
        self.version += 1
        self.row["updated_at"] = (self.now + timedelta(microseconds=self.version)).isoformat()

    def read_account(self, _db, aid):
        self.account_reads += 1
        return copy.deepcopy(self.row) if self.row and aid == ACCOUNT_ID else None

    def make_controller(self, *, real_http=False):
        self.monitor = OAuthMonitor(self.settings, self.db, base_url_provider=lambda: self.base_url,
            inventory_loader=lambda _db: [copy.deepcopy(self.row)], account_reader=self.read_account,
            usage_runner=execute_oauth_usage_query if real_http else self.usage_runner,
            test_runner=execute_sub2api_account_test if real_http else self.account_test_runner,
            recovery_runner=self.recovery_runner, clock=lambda: self.now)
        self.store = self.monitor.store
        if not self.store.admin_token():
            self.store.save_admin_token(ADMIN_TOKEN)
        self.controller = AutoResetController(self.monitor,
            request_runner=execute_credit_request if real_http else self.request_runner,
            schedule_runner=self.schedule_runner, evidence_reader=upstream_evidence)
        return self.controller

    def request_runner(self, action, aid, **connection):
        self.assertEqual(aid, ACCOUNT_ID)
        self.assertEqual(connection["admin_token"], ADMIN_TOKEN)
        self.calls.append(action)
        persisted = json.loads(self.store.path.read_text())["scheduler"][str(aid)]
        self.assertTrue(persisted["quota_query"]["last_query_at"])
        if action == "reset":
            self.assertIn(persisted["auto_reset_credit"]["stage"], ("resetting", "uncertain"))
        if self.request_hook:
            self.request_hook(action)
        if self.request_results:
            result = self.request_results.popleft()
            if isinstance(result, Exception):
                raise result
            return copy.deepcopy(result)
        count = 0 if action == "reset" else self.query_credits
        self.row["extra"]["codex_reset_credit_snapshot"] = {"available_count": count, "fetched_at": self.now.isoformat()}
        self.touch()
        return {"success": True, "consumed": action == "reset", "uncertain": False, "error_code": "",
                "data": {"code": "success", "windows_reset": 1 if action == "reset" else 0,
                         "quota": wham(self.now, five=self.reset_five if action == "reset" else self.query_five,
                                       seven=self.reset_seven if action == "reset" else self.query_seven)}}

    def schedule_runner(self, aid, enabled, **_connection):
        self.calls.append("schedule:" + str(enabled))
        self.row["schedulable"] = enabled
        self.touch()
        if self.schedule_hook:
            self.schedule_hook(enabled)
        return {"success": True}

    def account_test_runner(self, aid, model, **_connection):
        self.assertEqual(model, "fixture-recovery-model")
        self.calls.append("test")
        if self.test_hook:
            self.test_hook()
        return copy.deepcopy(self.test_results.popleft() if self.test_results else {"success": True})

    def recovery_runner(self, aid, **_connection):
        self.calls.append("recover")
        for name in ("rate_limited_at", "rate_limit_reset_at", "temp_unschedulable_until", "overload_until"):
            self.row[name] = None
        self.row["temp_unschedulable_reason"] = ""
        self.row["status"] = "active"
        self.row["error_message"] = ""
        self.touch()
        if self.recovery_hook:
            self.recovery_hook()
        return {"success": True}

    def usage_runner(self, aid, *_args, **_kwargs):
        self.calls.append("usage")
        return quota_result(wham(self.now), self.row, self.now)

    def run_once(self):
        self.controller.run([copy.deepcopy(self.row)], self.now)

    def step(self):
        self.controller._step(ACCOUNT_ID, self.now, self.settings.oauth_auto_reset_credit_enabled)

    def state(self):
        return self.controller.state(ACCOUNT_ID)

    def query_state(self):
        return self.store.scheduler().get(ACCOUNT_ID, {}).get("quota_query", {})


class AutoResetEligibilityTests(AutoResetFixture):
    def test_raw_weekly_percent_below_100_never_consumes_despite_full_five_hour(self):
        for used, expected in ((99.99, 0), (99.9999, 0), (100, 1), (100.01, 1)):
            with self.subTest(used=used):
                self.fresh_case()
                self.row["extra"].update(codex_5h_used_percent=100, codex_7d_used_percent=used)
                self.run_once()
                self.assertEqual(self.calls.count("reset"), expected)
                if not expected:
                    self.assertEqual(self.calls, [])

    def test_account_and_current_future_limit_conditions_are_jointly_required(self):
        changes = [{"platform": "grok"}, {"type": "apikey"}, {"status": "error"}, {"schedulable": False},
                   {"deleted_at": NOW.isoformat()}, {"parent_account_id": 8},
                   {"rate_limit_reset_at": None}, {"rate_limit_reset_at": NOW.isoformat()},
                   {"rate_limited_at": None}, {"temp_unschedulable_reason": "manual pause"},
                   {"expires_at": NOW.isoformat(), "auto_pause_on_expired": True}]
        for change in changes:
            with self.subTest(change=change):
                self.fresh_case()
                self.row.update(change)
                self.run_once()
                self.assertEqual(self.calls, [])
        self.fresh_case()
        self.row["extra"]["parent_account_id"] = 8
        self.run_once()
        self.assertEqual(self.calls, [])

    def test_current_upstream_429_excludes_client_throttle_history_and_newer_success_or_auth(self):
        for case in ("missing", "client_429", "historical", "future", "other_account", "new_success", "new_401", "new_402"):
            with self.subTest(case=case):
                self.fresh_case()
                self.db.raw.execute("DELETE FROM ops_error_logs")
                if case == "client_429":
                    self.db.event(upstream=200, status=429)
                elif case == "historical":
                    self.db.event(at=NOW - timedelta(hours=1))
                elif case == "future":
                    self.db.event(at=NOW + timedelta(seconds=1))
                elif case == "other_account":
                    self.db.event(account_id=8)
                elif case.startswith("new_"):
                    self.db.event()
                    if case == "new_success":
                        self.db.raw.execute("INSERT INTO usage_logs VALUES (?,?,?)", (2, ACCOUNT_ID, (NOW - timedelta(minutes=1)).isoformat()))
                    else:
                        self.db.event(2, at=NOW - timedelta(minutes=1), upstream=int(case[-3:]))
                self.run_once()
                self.assertEqual(self.calls, [])

    def test_weekly_evidence_requires_a_valid_observation_and_future_reset(self):
        for extra in ({"codex_7d_used_percent": None}, {"codex_7d_used_percent": 0},
                      {"codex_7d_reset_at": NOW.isoformat()}, {"codex_7d_reset_at": "bad"},
                      {"codex_usage_updated_at": "bad"},
                      {"codex_usage_updated_at": (NOW + timedelta(seconds=1)).isoformat()}):
            with self.subTest(extra=extra):
                self.fresh_case()
                self.row["extra"].update(extra)
                self.run_once()
                self.assertEqual(self.calls, [])

    def test_no_or_expired_credit_can_check_but_never_pause_consume_or_test(self):
        for snapshot in ({"available_count": 0}, {"available_count": 1, "credits": [{"expires_at": NOW.isoformat()}]}):
            with self.subTest(snapshot=snapshot):
                self.fresh_case()
                self.row["extra"]["codex_reset_credit_snapshot"] = {"fetched_at": (NOW - timedelta(hours=2)).isoformat(), **snapshot}
                self.query_credits = 0
                self.run_once()
                self.assertEqual(self.calls, ["query"])
                self.assertEqual(self.state()["error_code"], "no_credit")
                self.assertTrue(self.row["schedulable"])

    def test_original_upstream_auto_switch_conflicts_without_any_action(self):
        self.row["extra"]["auto_reset_credit_enabled"] = True
        self.run_once()
        self.assertEqual(self.calls, [])
        self.assertEqual(self.state()["error_code"], "conflict")

    def test_disabled_feature_and_30_second_discovery_gate(self):
        self.settings.oauth_auto_reset_credit_enabled = False
        self.run_once()
        self.assertEqual(self.calls, [])
        self.settings.oauth_auto_reset_credit_enabled = True
        self.now += timedelta(seconds=29)
        self.run_once()
        self.assertEqual(self.calls, [])
        self.now += timedelta(seconds=1)
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)

    def test_existing_auth_pause_prevents_card_requests(self):
        for code in ("http_401", "http_402"):
            with self.subTest(code=code):
                self.fresh_case()
                self.store.commit(scheduler_updates={ACCOUNT_ID: {"last_error_code": code,
                    "quota_query": {"auth_fingerprint": credential_fingerprint(self.row)}}})
                self.run_once()
                self.assertEqual(self.calls, [])

    def test_card_observed_at_start_of_current_weekly_window_can_be_consumed(self):
        self.row["extra"]["codex_reset_credit_snapshot"]["fetched_at"] = (NOW - timedelta(days=2)).isoformat()
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("query"), 0)


class AutoResetLifecycleTests(AutoResetFixture):
    def test_consumption_is_persisted_before_immediate_test_and_happens_once(self):
        def inspect_test():
            task = json.loads(self.store.path.read_text())["scheduler"][str(ACCOUNT_ID)]["auto_reset_credit"]
            self.assertTrue(task["consumed"])
            self.assertEqual(task["stage"], "testing")
            self.assertFalse(self.row["schedulable"])
            self.assertEqual(self.now, NOW)
        self.test_hook = inspect_test
        self.run_once()
        self.assertEqual(self.calls, ["schedule:False", "reset", "test", "recover", "schedule:True"])
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 1)
        self.assertTrue(self.monitor._cycle_tests[ACCOUNT_ID]["success"])
        self.assertEqual(len(self.store.snapshot()["recovery_history"]), 1)
        self.assertEqual(len(self.store.pending_events()), 1)
        self.assertNotIn(ADMIN_TOKEN, Path(self.settings.audit_path).read_text())
        self.make_controller()
        self.now += timedelta(hours=1)
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

    def test_failed_tests_back_off_one_five_fifteen_thirty_minutes_without_new_consumption(self):
        self.test_results.extend({"success": False, "error_code": "http_502"} for _ in range(6))
        self.run_once()
        for attempt, delay in enumerate((60, 300, 900, 1800, 1800), 1):
            with self.subTest(attempt=attempt):
                self.assertEqual(self.state()["test_attempts"], attempt)
                due = self.now + timedelta(seconds=delay)
                self.assertEqual(self.state()["next_at"], due.isoformat())
                self.now = due - timedelta(microseconds=1)
                self.step()
                self.assertEqual(self.calls.count("test"), attempt)
                self.now = due
                self.step()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("query"), 0)
        self.assertEqual(self.calls.count("test"), 6)
        self.assertEqual(self.calls.count("schedule:True"), 0)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 1)

    def test_successful_test_does_not_release_with_other_required_window_exhausted(self):
        self.reset_five = 100
        self.run_once()
        self.assertEqual(self.calls, ["schedule:False", "reset", "test"])
        self.assertEqual(self.state()["stage"], "confirming")
        self.assertFalse(self.row["schedulable"])

    def test_config_change_during_pause_prevents_consumption(self):
        self.schedule_hook = lambda _enabled: self.row.update(concurrency=2)
        self.run_once()
        self.assertEqual(self.calls, ["schedule:False"])
        self.assertEqual(self.state()["stage"], "blocked")

    def test_manual_change_during_retry_relinquishes_pause_ownership(self):
        self.test_results.append({"success": False, "error_code": "http_502"})
        self.run_once()
        self.touch()
        self.now += timedelta(minutes=1)
        self.step()
        self.assertEqual(self.state()["stage"], "blocked")
        self.assertFalse(self.state()["owns_pause"])
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(self.calls.count("schedule:True"), 0)

    def test_manual_cancel_during_test_cannot_be_overwritten_or_resume_account(self):
        self.test_hook = lambda: self.controller.cancel(ACCOUNT_ID)
        self.run_once()
        self.assertEqual(self.state()["stage"], "manual")
        self.assertFalse(self.state()["owns_pause"])
        self.assertEqual(self.calls.count("schedule:True"), 0)
        self.assertFalse(self.row["schedulable"])

    def test_test_auth_errors_wait_for_credential_change_and_never_consume_again(self):
        for code in ("http_401", "http_402"):
            with self.subTest(code=code):
                self.fresh_case()
                self.test_results.append({"success": False, "error_code": code})
                self.run_once()
                self.assertEqual(self.state()["error_code"], "auth_paused")
                self.now += timedelta(seconds=30)
                self.step()
                self.assertEqual(self.calls.count("test"), 1)
                self.row["credentials"]["access_token"] = "fixture-access-v2"
                self.touch()
                self.step()
                self.assertEqual(self.calls.count("test"), 2)
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.state()["stage"], "recovered")

    def test_card_query_auth_pause_recovers_after_credentials_change_without_owned_hold(self):
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        self.request_results.append({"success": False, "error_code": "http_401"})
        self.run_once()
        self.assertEqual(self.state()["error_code"], "auth_paused")
        self.assertTrue(self.row["schedulable"])
        self.row["credentials"]["access_token"] = "fixture-access-v2"
        self.now += timedelta(hours=1)
        self.touch()
        self.step()
        self.assertEqual(self.calls.count("query"), 2)
        self.assertNotEqual(self.state()["error_code"], "auth_paused")

    def test_test_auth_error_status_keeps_auth_pause_until_credentials_are_repaired(self):
        for code, status in (("http_401", "error"), ("http_402", "error"), ("http_401", "active")):
            with self.subTest(code=code, repaired_status=status):
                self.fresh_case()
                self.test_results.append({"success": False, "error_code": code})
                def upstream_marks_auth_error():
                    self.row["status"] = "error"
                    self.row["error_message"] = "fixture upstream credential rejected"
                    self.touch()
                self.test_hook = upstream_marks_auth_error
                self.run_once()
                self.assertEqual(self.state()["error_code"], "auth_paused")
                self.assertEqual(self.calls.count("schedule:True"), 0)
                self.assertFalse(self.row["schedulable"])
                self.test_hook = None
                self.now += timedelta(seconds=30)
                self.step()
                self.assertEqual(self.calls.count("test"), 1)
                self.row["credentials"]["access_token"] = "fixture-access-v2"
                self.row["status"] = status
                self.touch()
                self.step()
                self.assertEqual(self.calls.count("test"), 2)
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.state()["stage"], "recovered")

    def test_auth_credential_update_cannot_override_new_error_or_manual_disable(self):
        for change in ({"status": "disabled"}, {"error_message": "unrelated manual account error"}):
            with self.subTest(change=change):
                self.fresh_case()
                self.test_results.append({"success": False, "error_code": "http_401"})
                def upstream_marks_auth_error():
                    self.row.update(status="error", error_message="fixture upstream credential rejected")
                    self.touch()
                self.test_hook = upstream_marks_auth_error
                self.run_once()
                self.assertEqual(self.state()["error_code"], "auth_paused")
                self.test_hook = None
                self.row["credentials"]["access_token"] = "fixture-access-v2"
                self.row.update(change)
                self.touch()
                self.now += timedelta(seconds=30)
                self.step()
                self.assertEqual(self.calls.count("test"), 1)
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.calls.count("recover"), 0)
                self.assertEqual(self.calls.count("schedule:True"), 0)
                self.assertFalse(self.row["schedulable"])

    def test_manual_disable_during_test_or_recovery_never_restores_scheduling(self):
        for boundary in ("test", "recovery"):
            with self.subTest(boundary=boundary):
                self.fresh_case()
                def disable():
                    self.row["status"] = "disabled"
                    self.touch()
                if boundary == "test":
                    self.test_hook = disable
                else:
                    self.recovery_hook = disable
                self.run_once()
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.calls.count("test"), 1)
                self.assertEqual(self.calls.count("schedule:True"), 0)
                self.assertFalse(self.row["schedulable"])
                self.assertEqual(self.row["status"], "disabled")

    def test_restart_preserves_retry_deadline_and_consumption(self):
        self.test_results.append({"success": False, "error_code": "http_502"})
        self.run_once()
        episode = self.state()["episode"]
        self.make_controller()
        self.now += timedelta(seconds=59)
        self.step()
        self.assertEqual(self.calls.count("test"), 1)
        self.now += timedelta(seconds=1)
        self.step()
        self.assertEqual(self.state()["episode"], episode)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 2)
        self.assertEqual(self.state()["stage"], "recovered")

    def test_timeout_is_never_replayed_after_restart_and_confirmation_shares_cooldown(self):
        self.request_results.append(TimeoutError("isolated timeout"))
        self.run_once()
        self.assertEqual(self.calls, ["schedule:False", "reset"])
        self.make_controller()
        self.now += timedelta(hours=1)
        self.step()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("query"), 0)
        self.query_seven = 20
        self.now += timedelta(seconds=180)
        self.step()
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertFalse(self.state().get("consumed", False))

    def test_state_write_failure_before_reservation_prevents_all_actions(self):
        with patch.object(self.store, "_write", side_effect=OSError("fixture disk full")):
            self.run_once()
        self.assertEqual(self.calls, [])
        self.assertTrue(self.row["schedulable"])

    def test_state_write_failure_after_pause_never_sends_reset_or_restores_unknown_hold(self):
        write = self.store._write
        def fail_reset_intent(data):
            task = data["scheduler"].get(str(ACCOUNT_ID), {}).get("auto_reset_credit", {})
            if task.get("stage") == "resetting":
                raise OSError("fixture cannot persist reset intent")
            return write(data)
        with patch.object(self.store, "_write", side_effect=fail_reset_intent):
            self.run_once()
        self.make_controller()
        self.now += timedelta(hours=2)
        self.step()
        self.assertEqual(self.calls, ["schedule:False"])
        self.assertEqual(self.state()["stage"], "blocked")

    def test_consumption_result_write_failure_survives_restart_without_second_reset(self):
        write = self.store._write
        failed = []
        def fail_consumed_once(data):
            task = data["scheduler"].get(str(ACCOUNT_ID), {}).get("auto_reset_credit", {})
            if task.get("consumed") and not failed:
                failed.append(True)
                raise OSError("fixture cannot persist consumed response")
            return write(data)
        with patch.object(self.store, "_write", side_effect=fail_consumed_once):
            self.run_once()
        self.assertEqual(failed, [True])
        self.make_controller()
        self.now += timedelta(hours=2)
        self.step()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 0)
        self.assertEqual(self.calls.count("schedule:True"), 0)

    def test_quota_result_write_failure_keeps_confirmed_consumption_on_disk(self):
        write = self.store._write
        failed = []
        def fail_quota_once(data):
            if data["oauth_results"] and not failed:
                failed.append(True)
                raise OSError("fixture cannot persist attached quota")
            return write(data)
        with patch.object(self.store, "_write", side_effect=fail_quota_once):
            self.run_once()
        self.assertEqual(failed, [True])
        self.assertTrue(self.state()["consumed"])
        self.make_controller()
        self.now += timedelta(hours=1)
        self.step()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertTrue(self.state()["consumed"])

    def test_two_controllers_share_disk_lock_and_only_one_consumption(self):
        first = self.controller
        second = self.make_controller()
        entered, release = threading.Event(), threading.Event()
        errors = []
        def block(action):
            if action == "reset":
                entered.set()
                if not release.wait(3):
                    raise AssertionError("fixture reset release timed out")
        self.request_hook = block
        def run_first():
            try:
                first.run([copy.deepcopy(self.row)], self.now)
            except BaseException as exc:
                errors.append(exc)
        worker = threading.Thread(target=run_first)
        worker.start()
        try:
            self.assertTrue(entered.wait(2))
            second.run([copy.deepcopy(self.row)], self.now)
        finally:
            release.set()
            worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

    def test_manual_reset_updates_shared_state_without_launching_an_automatic_test(self):
        result = self.controller.manual(copy.deepcopy(self.row), "reset", ADMIN_TOKEN)
        self.assertTrue(result["consumed"])
        self.assertEqual(self.calls, ["reset"])
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(self.query_state().get("automatic_attempts", []), [])
        self.assertEqual(self.query_state()["last_query_at"], self.now.isoformat())
        self.assertTrue(self.row["schedulable"])
        self.step()
        self.assertEqual(self.calls, ["reset"])

    def test_uncertain_manual_reset_cannot_repeat_or_be_taken_over_automatically(self):
        self.request_results.append({"success": False, "uncertain": True, "error_code": "result_uncertain"})
        self.controller.manual(copy.deepcopy(self.row), "reset", ADMIN_TOKEN)
        result = self.controller.manual(copy.deepcopy(self.row), "reset", ADMIN_TOKEN)
        self.assertFalse(result["success"])
        self.make_controller()
        self.now += timedelta(hours=2)
        self.step()
        self.assertEqual(self.calls, ["reset"])


class LocalCreditServer:
    def __init__(self, fixture):
        self.requests = []
        self.quota_reads = 0
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def respond(self):
                payload = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                owner.requests.append((self.command, self.path, self.headers.get("x-api-key")))
                if self.path.endswith("/test"):
                    result = fixture.account_test_runner(ACCOUNT_ID, json.loads(payload)["model_id"])
                    event = {"type": "test_complete", "success": True} if result["success"] else {
                        "type": "error", "code": result.get("error_code"), "message": "isolated failure"}
                    body, content_type = ("data: " + json.dumps(event) + "\n\n").encode(), "text/event-stream"
                elif "/usage?" in self.path:
                    owner.quota_reads += 1
                    body = json.dumps({"data": {
                        "five_hour": {"utilization": 10, "resets_at": (fixture.now + timedelta(hours=5)).isoformat()},
                        "seven_day": {"utilization": 20, "resets_at": (fixture.now + timedelta(days=7)).isoformat()}}}).encode()
                    content_type = "application/json"
                else:
                    owner.quota_reads += 1
                    action = "reset" if self.path.endswith("/reset-quota") else "query"
                    result = fixture.request_runner(action, ACCOUNT_ID, admin_token=ADMIN_TOKEN)
                    body, content_type = json.dumps({"code": 0, "data": result["data"]}).encode(), "application/json"
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            do_GET = respond
            do_POST = respond

            def log_message(self, *_args):
                pass
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        fixture.allowed_ports.add(self.server.server_port)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)


class AutoResetHttpBudgetTests(AutoResetFixture):
    def setUp(self):
        super().setUp()
        self.server = LocalCreditServer(self)
        self.addCleanup(self.server.close)
        self.base_url = self.server.base_url
        self.make_controller(real_http=True)

    def test_real_consumption_http_has_one_attached_quota_read_and_immediate_sse_test(self):
        self.run_once()
        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/reset-quota"),
            ("POST", "/api/v1/admin/accounts/7/test")])
        self.assertTrue(all(key == ADMIN_TOKEN for _, _, key in self.server.requests))
        self.assertEqual(self.server.quota_reads, 1)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 1)
        self.assertEqual(self.state()["stage"], "recovered")

    def test_card_refresh_consume_and_usage_share_hourly_cooldown_and_six_read_budget(self):
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        self.run_once()
        self.assertEqual(self.calls, ["query"])
        self.assertEqual(self.server.quota_reads, 1)
        seven = next(window for window in self.store.result(ACCOUNT_ID)["oauth_quota"]["ui_windows"]
                     if window["key"] == "codex_7d")
        self.assertEqual(seven["used_percent"], 100)
        self.now += timedelta(seconds=3599)
        self.step()
        self.assertEqual(self.server.quota_reads, 1)
        self.now += timedelta(seconds=1)
        self.step()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(self.server.quota_reads, 2)
        for offset in range(2, 6):
            self.now = NOW + timedelta(hours=offset)
            result = self.monitor.queries.query(copy.deepcopy(self.row), ADMIN_TOKEN, source="automatic", reason="fixture-other-entry")
            self.assertTrue(result["success"], result)
        self.assertEqual(self.server.quota_reads, 6)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 6)
        self.now = NOW + timedelta(hours=6)
        result = self.monitor.queries.query(copy.deepcopy(self.row), ADMIN_TOKEN, source="automatic", reason="fixture-other-entry")
        self.assertEqual(result["error_code"], "query_budget")
        self.assertEqual(self.server.quota_reads, 6)
        self.make_controller(real_http=True)
        self.now = NOW + timedelta(hours=24) - timedelta(microseconds=1)
        result = self.monitor.queries.query(copy.deepcopy(self.row), ADMIN_TOKEN, source="automatic", reason="fixture-restart")
        self.assertEqual(result["error_code"], "query_budget")
        self.now += timedelta(microseconds=1)
        self.assertTrue(self.monitor.queries.query(copy.deepcopy(self.row), ADMIN_TOKEN, source="automatic", reason="fixture-restart")["success"])
        self.assertEqual(self.server.quota_reads, 7)

    def test_confirmed_consumption_with_cache_warning_still_tests_immediately(self):
        self.request_results.append({"success": True, "consumed": True, "uncertain": False,
            "data": {"code": "success", "windows_reset": 1, "cache_refreshed": False,
                     "cache_persisted": False, "warning_code": "cache_failed"}})
        self.run_once()
        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/reset-quota"),
            ("POST", "/api/v1/admin/accounts/7/test")])
        self.assertTrue(self.state()["consumed"])
        self.assertEqual(self.state()["stage"], "confirming")
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(self.server.quota_reads, 1)
        self.assertEqual(self.calls.count("schedule:True"), 0)


if __name__ == "__main__":
    unittest.main()
