from __future__ import annotations

import copy
import io
import json
import re
import socket
import sqlite3
import tempfile
import threading
import urllib.error
import unittest
from collections import deque
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from app.auto_reset import AutoResetController, execute_credit_request, quota_result, upstream_evidence
from app.oauth_monitor import OAuthMonitor, execute_sub2api_account_test
from app.oauth_queries import credential_fingerprint
from app.reset_credit_observation import receipt_diagnostics
from app.settings import Settings
from app.usage_query import execute_oauth_usage_query


NOW = datetime(2026, 9, 30, 8, tzinfo=timezone.utc)
ACCOUNT_ID = 7
ADMIN_TOKEN = "auto-reset-isolated-fixture-admin"


def wham(now, *, five=10, seven=20, observed=None, five_reset_at=None, seven_reset_at=None):
    return {"fetched_at": (observed or now).isoformat(), "rate_limit": {
        "primary_window": {"limit_window_seconds": 18000, "used_percent": five,
                           "reset_at": int((five_reset_at or now + timedelta(hours=5)).timestamp())},
        "secondary_window": {"limit_window_seconds": 604800, "used_percent": seven,
                             "reset_at": int((seven_reset_at or now + timedelta(days=5)).timestamp())}}}


def native_credit_snapshot(now, *, count=1, expiries=None):
    if expiries is None:
        expiries = [now + timedelta(days=2)] if count else []
    return {"available_count": count,
            "credits": [{"expires_at": value.isoformat()} for value in expiries]}


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
        from app.error_evidence import local_throttle_sql
        predicate = local_throttle_sql()
        if "to_jsonb(e)" in sql or predicate in sql:
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
        self.query_seven_reset_at = NOW + timedelta(days=5)
        self.reset_seven_reset_at = NOW + timedelta(days=5)
        self.native_credit_response = None
        self.native_credit_db_snapshot = None
        self.native_credit_fetched_at = None
        self.native_credit_cache_persisted = True
        self.native_credit_cache_refreshed = True
        self.http_status_overrides = {}
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
        if action == "query" and self.native_credit_response is not None:
            response_snapshot = copy.deepcopy(self.native_credit_response)
            database_snapshot = (self.native_credit_db_snapshot if self.native_credit_db_snapshot is not None
                                 else response_snapshot)
            self.row["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(database_snapshot)
            self.touch()
            data = {**wham(self.now, five=self.query_five, seven=self.query_seven,
                           seven_reset_at=self.query_seven_reset_at),
                    "code": "success", "windows_reset": 0,
                    "rate_limit_reset_credits": response_snapshot,
                    "fetched_at": (self.native_credit_fetched_at if self.native_credit_fetched_at is not None
                                   else int(self.now.timestamp())),
                    "cache_persisted": self.native_credit_cache_persisted,
                    "cache_refreshed": self.native_credit_cache_refreshed}
            return {"success": True, "consumed": False, "uncertain": False, "error_code": "", "data": data}
        if action == "reset" and self.native_credit_response is not None:
            self.row["extra"]["codex_reset_credit_snapshot"] = {"available_count": 0, "credits": []}
            self.touch()
            data = {**wham(self.now, five=self.reset_five, seven=self.reset_seven,
                           seven_reset_at=self.reset_seven_reset_at),
                    "code": "success", "windows_reset": 1,
                    "cache_refreshed": self.native_credit_cache_refreshed,
                    "cache_persisted": self.native_credit_cache_persisted}
            return {"success": True, "consumed": True, "uncertain": False, "error_code": "", "data": data}
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

    def assert_credit_recovery(self, *, method="connection", episode=None):
        task = self.state()
        self.assertEqual(task["stage"], "recovered")
        self.assertTrue(task["consumed"])
        if episode is not None:
            self.assertEqual(task["episode"], episode)
        history = self.store.snapshot()["recovery_history"]
        self.assertEqual(list(history), [f"reset-credit:{ACCOUNT_ID}:{task['episode']}"])
        record = next(iter(history.values()))
        self.assertEqual(record["kind"], "reset_credit")
        expected = {"consumed": True, "completed_at": task["reset_completed_at"],
                    "verification_method": method}
        self.assertEqual(record["reset_credit"], expected)
        events = self.store.pending_events()
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["status"], "recovered")
        self.assertEqual(events[0]["dedupe_key"], record["dedupe_key"])
        self.assertEqual(events[0]["reset_credit"], expected)


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

    def test_active_account_block_fallback_is_used_only_without_explicit_blockers(self):
        for case in ("missing", "historical", "future", "other_account"):
            with self.subTest(case=case):
                self.fresh_case()
                self.db.raw.execute("DELETE FROM ops_error_logs")
                if case == "historical":
                    self.db.event(at=NOW - timedelta(hours=1))
                elif case == "future":
                    self.db.event(at=NOW + timedelta(seconds=1))
                elif case == "other_account":
                    self.db.event(account_id=8)
                self.run_once()
                self.assertEqual(self.calls.count("reset"), 1)
                task = self.state()
                self.assertEqual(task["evidence_source"], "account_rate_limit")
                self.assertTrue(task["evidence_fingerprint"])
                self.assertNotIn("fixture-access-v1", json.dumps(task))

        self.fresh_case()
        self.db.raw.execute("INSERT INTO usage_logs VALUES (?,?,?)", (2, ACCOUNT_ID, (NOW - timedelta(minutes=1)).isoformat()))
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.state()["evidence_source"], "upstream_error")

        for case in ("client_429", "new_401", "new_402"):
            with self.subTest(case=case):
                self.fresh_case()
                self.db.raw.execute("DELETE FROM ops_error_logs")
                if case == "client_429":
                    self.db.event(error_owner="client", error_phase="request")
                elif case.startswith("new_"):
                    self.db.event()
                    self.db.event(2, at=NOW - timedelta(minutes=1), upstream=int(case[-3:]))
                self.run_once()
                self.assertEqual(self.calls, [])

    def test_actionable_upstream_error_is_saved_as_primary_evidence(self):
        self.run_once()
        task = self.state()
        self.assertEqual(task["evidence_source"], "upstream_error")
        self.assertEqual(task["evidence_id"], "1")
        self.assertTrue(task["evidence_fingerprint"])
        self.assertNotIn("fixture-access-v1", json.dumps(task))

    def test_account_rate_limit_fallback_requires_same_window_exhaustion_and_live_state(self):
        changes = [
            {"rate_limit_reset_at": NOW.isoformat()},
            {"rate_limited_at": None},
            {"expires_at": (NOW - timedelta(seconds=1)).isoformat(), "auto_pause_on_expired": True},
        ]
        extra_changes = [
            {"codex_7d_used_percent": 99.999},
            {"codex_usage_updated_at": (NOW - timedelta(days=8)).isoformat()},
            {"codex_usage_updated_at": (NOW + timedelta(seconds=1)).isoformat()},
        ]
        for change in changes:
            with self.subTest(change=change):
                self.fresh_case()
                self.db.raw.execute("DELETE FROM ops_error_logs")
                self.row.update(change)
                self.run_once()
                self.assertEqual(self.calls, [])
        for change in extra_changes:
            with self.subTest(extra=change):
                self.fresh_case()
                self.db.raw.execute("DELETE FROM ops_error_logs")
                self.row["extra"].update(change)
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
        event = self.store.pending_events()[0]
        self.assertEqual(event["reset_credit"]["consumed"], True)
        self.assertEqual(event["reset_credit"]["completed_at"], self.state()["reset_completed_at"])
        self.assertIn(event["reset_credit"]["verification_method"], {"connection", "model"})
        self.assert_credit_recovery()
        self.assertNotIn(ADMIN_TOKEN, Path(self.settings.audit_path).read_text())
        self.assertNotIn("fixture-access-v1", Path(self.settings.usage_query_state_path).read_text())
        self.make_controller()
        self.now += timedelta(hours=1)
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

    def test_unknown_consumption_can_recover_without_use_card_event_metadata(self):
        self.request_results.append({"success": False, "consumed": False, "uncertain": True,
                                     "error_code": "result_uncertain"})
        self.run_once()
        self.assertEqual(self.state()["stage"], "uncertain")
        self.assertFalse(self.state()["consumed"])

        self.now += timedelta(hours=1)
        self.store.commit(results={ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now)})
        self.step()

        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        event = self.store.pending_events()[0]
        self.assertEqual(event["status"], "recovered")
        self.assertNotIn("reset_credit", event)

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

    def interrupt_release(self, boundary):
        failed = []
        write = self.store._write

        def fail_completion(data):
            task = data["scheduler"].get(str(ACCOUNT_ID), {}).get("auto_reset_credit", {})
            if boundary == "final_state_write" and task.get("stage") == "recovered" and not failed:
                failed.append(boundary)
                raise OSError("fixture final recovery write failed")
            return write(data)

        def fail_after_schedule(enabled):
            if boundary == "schedule_readback" and enabled and not failed:
                failed.append(boundary)
                raise OSError("fixture interrupted after scheduling changed")

        self.schedule_hook = fail_after_schedule
        with patch.object(self.store, "_write", side_effect=fail_completion):
            self.run_once()
        self.schedule_hook = None
        self.assertEqual(failed, [boundary])
        self.assertEqual(self.state()["stage"], "releasing")
        self.assertTrue(self.state()["consumed"])
        self.assertTrue(self.row["schedulable"])
        self.assertIsNone(self.row["rate_limited_at"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])
        return copy.deepcopy(self.state())

    def test_releasing_restart_finishes_same_history_without_repeating_mutations(self):
        for boundary in ("schedule_readback", "final_state_write"):
            with self.subTest(boundary=boundary):
                self.fresh_case()
                interrupted = self.interrupt_release(boundary)
                calls = list(self.calls)
                self.assertEqual(calls, ["schedule:False", "reset", "test", "recover", "schedule:True"])
                self.make_controller()
                self.now += timedelta(seconds=30)
                self.run_once()
                self.assert_credit_recovery(episode=interrupted["episode"])
                self.assertEqual(self.state()["reset_completed_at"], interrupted["reset_completed_at"])
                self.assertEqual(self.state()["receipt"], interrupted["receipt"])
                self.assertEqual(self.calls, calls)
                history = copy.deepcopy(self.store.snapshot()["recovery_history"])
                self.make_controller()
                self.now += timedelta(seconds=30)
                self.run_once()
                self.assertEqual(self.calls, calls)
                self.assertEqual(self.store.snapshot()["recovery_history"], history)
                self.assertEqual(len(self.store.pending_events()), 1)

    def test_releasing_restart_preserves_manual_changes_and_new_blocks(self):
        for change in ("disabled", "schedule_off", "configuration", "new_limit", "manual_generation"):
            with self.subTest(change=change):
                self.fresh_case()
                self.interrupt_release("final_state_write")
                self.now += timedelta(seconds=10)
                if change == "disabled":
                    self.row["status"] = "disabled"
                elif change == "schedule_off":
                    self.row["schedulable"] = False
                elif change == "configuration":
                    self.row["concurrency"] = 2
                elif change == "new_limit":
                    self.row["rate_limited_at"] = self.now.isoformat()
                    self.row["rate_limit_reset_at"] = (self.now + timedelta(hours=5)).isoformat()
                else:
                    self.store.manual_control(self.row, True, self.now)
                self.touch()
                expected_row, calls = copy.deepcopy(self.row), list(self.calls)
                self.make_controller()
                self.now += timedelta(seconds=30)
                self.run_once()
                if change != "manual_generation":
                    self.assertNotEqual(self.state()["stage"], "recovered")
                self.assertEqual(self.row, expected_row)
                self.assertEqual(self.calls, calls)
                self.assertEqual(self.store.snapshot()["recovery_history"], {})
                self.assertEqual(self.store.pending_events(), [])

    def cancel_waiting_task(self):
        self.store.update_scheduler({ACCOUNT_ID: {"quota_query": {"last_query_at": self.now.isoformat()}}})
        self.run_once()
        self.assertEqual(self.state()["stage"], "waiting")
        self.assertFalse(self.state().get("attempt_at"))
        self.assertEqual(self.calls, [])
        old_episode = self.state()["episode"]
        self.store.manual_control(self.row, False, self.now)
        self.row["schedulable"] = False
        self.touch()
        self.assertEqual(self.state()["stage"], "manual")
        return old_episode

    def new_limit(self):
        self.row["rate_limited_at"] = (self.now - timedelta(seconds=1)).isoformat()
        self.row["rate_limit_reset_at"] = (self.now + timedelta(days=5)).isoformat()
        self.row["extra"].update(codex_usage_updated_at=self.now.isoformat(), codex_7d_used_percent=100,
            codex_7d_reset_at=(self.now + timedelta(days=5)).isoformat(),
            codex_reset_credit_snapshot={"available_count": 1, "fetched_at": self.now.isoformat(),
                "credits": [{"expires_at": (self.now + timedelta(days=2)).isoformat()}]})
        self.touch()
        self.db.event(record_id=2, at=self.now - timedelta(seconds=1))

    def test_undispatched_manual_task_allows_new_limit_after_explicit_reopen(self):
        old_episode = self.cancel_waiting_task()
        self.now += timedelta(minutes=1)
        self.store.manual_control(self.row, True, self.now)
        self.row["schedulable"] = True
        self.touch()
        self.now += timedelta(hours=1)
        self.new_limit()
        self.make_controller()
        self.run_once()
        self.assertNotEqual(self.state()["episode"], old_episode)
        self.assertEqual(self.calls, ["schedule:False", "reset", "test", "recover", "schedule:True"])
        self.assert_credit_recovery()

    def test_undispatched_manual_task_requires_explicit_reopen_and_newer_evidence(self):
        for reopen in (False, True):
            with self.subTest(reopen=reopen):
                self.fresh_case()
                old_episode = self.cancel_waiting_task()
                self.now += timedelta(hours=2)
                if reopen:
                    self.store.manual_control(self.row, True, self.now)
                else:
                    self.new_limit()
                self.row["schedulable"] = True
                self.touch()
                self.make_controller()
                self.run_once()
                self.assertEqual(self.state()["episode"], old_episode)
                self.assertEqual(self.state()["stage"], "manual")
                self.assertEqual(self.calls, [])

    def test_dispatched_manual_task_requires_observed_recovery_before_new_episode(self):
        self.request_results.append({"success": False, "consumed": False, "uncertain": True,
                                     "error_code": "result_uncertain"})
        self.run_once()
        original = self.state()
        self.store.manual_control(self.row, False, self.now)
        self.now += timedelta(minutes=1)
        self.store.manual_control(self.row, True, self.now)
        self.row["schedulable"] = True
        self.touch()
        self.now += timedelta(hours=2)
        self.new_limit()
        self.make_controller()
        self.run_once()
        self.assertEqual(self.state()["episode"], original["episode"])
        self.assertEqual(self.state()["attempt_at"], original["attempt_at"])
        self.assertEqual(self.calls, ["schedule:False", "reset"])
        self.assertEqual(self.state()["stage"], "manual")
        self.now += timedelta(seconds=30)
        self.query_five, self.query_seven = 10, 20
        self.store.commit(results={ACCOUNT_ID: quota_result(wham(self.now), self.row, self.now)})
        self.run_once()
        self.assertEqual(self.state()["stage"], "manual")
        self.assertEqual(self.calls, ["schedule:False", "reset"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})

        self.row.update(rate_limited_at=None, rate_limit_reset_at=None, temp_unschedulable_until=None,
                        temp_unschedulable_reason="", overload_until=None, status="active", error_message="",
                        schedulable=True)
        self.touch()
        self.now += timedelta(seconds=31)
        self.run_once()
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(self.state()["episode"], original["episode"])
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.store.snapshot()["recovery_history"], {})

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
        self.assertEqual(self.state()["stage"], "manual")
        self.assertEqual(self.state()["consumption_outcome"], "consumed")
        self.assertTrue(self.state()["reset_completed_at"])
        self.assertEqual(self.calls, ["reset"])
        self.assertEqual(self.query_state().get("automatic_attempts", []), [])
        self.assertEqual(self.query_state()["last_query_at"], self.now.isoformat())
        self.assertTrue(self.row["schedulable"])

        self.query_five, self.query_seven = 10, 20
        self.now += timedelta(seconds=30)
        self.store.commit(results={ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now)})
        self.step()
        self.assertEqual(self.state()["stage"], "manual")
        self.assertEqual(self.calls, ["reset"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])

        self.row.update(rate_limited_at=None, rate_limit_reset_at=None, temp_unschedulable_until=None,
                        temp_unschedulable_reason="", overload_until=None, status="active", error_message="",
                        schedulable=True)
        self.touch()
        self.now += timedelta(seconds=30)
        self.store.commit(results={ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now)})
        self.step()
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(self.calls, ["reset"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])

    def test_uncertain_manual_task_requires_account_recovery_after_low_quota(self):
        self.request_results.append({"success": False, "consumed": False, "uncertain": True,
                                     "error_code": "result_uncertain"})
        result = self.controller.manual(copy.deepcopy(self.row), "reset", ADMIN_TOKEN)
        self.assertFalse(result["success"])
        task = self.state()
        self.assertEqual(task["stage"], "uncertain")
        self.assertEqual(task["consumption_outcome"], "unknown")
        self.assertFalse(task.get("owns_pause"))
        self.assertEqual(self.calls, ["reset"])

        self.query_five, self.query_seven = 10, 20
        self.now += timedelta(seconds=30)
        self.store.commit(results={ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now)})
        self.step()
        self.assertEqual(self.state()["stage"], "uncertain")
        self.assertEqual(self.state()["episode"], task["episode"])
        self.assertEqual(self.calls, ["reset"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])

        self.row.update(rate_limited_at=None, rate_limit_reset_at=None, temp_unschedulable_until=None,
                        temp_unschedulable_reason="", overload_until=None, status="active", error_message="",
                        schedulable=True)
        self.touch()
        self.now += timedelta(seconds=30)
        self.store.commit(results={ACCOUNT_ID: quota_result(wham(self.now, five=10, seven=20), self.row, self.now)})
        self.step()
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(self.state()["episode"], task["episode"])
        self.assertEqual(self.calls, ["reset"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])

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
                status = 200
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
                    override = fixture.http_status_overrides.get(action)
                    if override:
                        status, error_body = override
                        body, content_type = json.dumps(error_body).encode(), "application/json"
                    else:
                        result = fixture.request_runner(action, ACCOUNT_ID, admin_token=ADMIN_TOKEN)
                        body, content_type = json.dumps({"code": 0, "data": result["data"]}).encode(), "application/json"
                self.send_response(status)
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

    def native_observation(self):
        scheduler = self.store.scheduler()
        account = scheduler.get(ACCOUNT_ID) or scheduler.get(str(ACCOUNT_ID)) or {}
        return account.get("reset_credit_observation")

    def use_native_credit_response(self, snapshot, *, database_snapshot=None, cache_persisted=True):
        self.native_credit_response = copy.deepcopy(snapshot)
        self.native_credit_db_snapshot = copy.deepcopy(database_snapshot if database_snapshot is not None else snapshot)
        self.native_credit_cache_persisted = cache_persisted
        self.row["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(self.native_credit_db_snapshot)

    def test_native_reset_receipt_recovers_through_scheduled_monitor_with_one_merged_event(self):
        self.use_native_credit_response(native_credit_snapshot(NOW))
        self.assertNotIn("fetched_at", self.row["extra"]["codex_reset_credit_snapshot"])
        request = self.request_runner

        def native_request(action, aid, **connection):
            result = request(action, aid, **connection)
            data = result["data"]
            data["fetched_at"] = int(self.now.timestamp())
            data["quota"] = {"rate_limit": data.pop("rate_limit"), "fetched_at": data["fetched_at"]}
            if action == "reset":
                data["code"] = "reset"
                data["windows_reset"] = 2
            return result

        self.request_runner = native_request
        self.monitor.auto_reset = self.controller
        self.monitor.run_once(now=self.now)
        self.assertEqual(self.calls, ["query"])
        self.assertEqual(self.native_observation()["observed_at"], NOW.isoformat())
        self.assertEqual(self.state()["stage"], "waiting")
        self.assertFalse(self.state().get("attempt_at"))
        self.make_controller(real_http=True)
        self.monitor.auto_reset = self.controller
        self.now += timedelta(hours=1)

        def persisted_before_test():
            self.assertEqual(self.now, NOW + timedelta(hours=1))
            self.assertTrue(self.state()["consumed"])
            self.assertEqual(self.state()["receipt"]["business_code"], "reset")
            self.assertFalse(self.row["schedulable"])

        self.test_hook = persisted_before_test
        self.monitor.run_once(now=self.now)
        self.assertEqual(self.calls, ["query", "schedule:False", "reset", "test", "recover", "schedule:True"])
        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/quota/refresh"),
            ("POST", "/api/v1/admin/openai/accounts/7/reset-quota"),
            ("POST", "/api/v1/admin/accounts/7/test")])
        self.assert_credit_recovery()
        self.assertEqual(self.state()["receipt"]["windows_reset"], 2)
        self.assertEqual(self.server.quota_reads, 2)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 2)
        history = copy.deepcopy(self.store.snapshot()["recovery_history"])
        self.make_controller(real_http=True)
        self.monitor.auto_reset = self.controller
        self.now += timedelta(seconds=30)
        self.monitor.run_once(now=self.now)
        self.assertEqual(self.store.snapshot()["recovery_history"], history)
        self.assertEqual(len(self.store.pending_events()), 1)
        self.assertEqual(len(self.server.requests), 3)

    def test_native_unknown_and_contradictory_receipts_are_not_replayed_after_restart(self):
        cases = (("already_redeemed", 0), ("already_redeemed", 1), ("unrecognized_fixture", 1),
                 ("no_credit", 1), ("nothing_to_reset", 1), ("reset", 0), ("reset", True))
        for code, windows in cases:
            with self.subTest(code=code, windows=windows):
                self.fresh_case()
                self.server.requests.clear()
                self.base_url = self.server.base_url
                self.make_controller(real_http=True)
                self.request_results.append({"data": {"code": code, "windows_reset": windows}})
                self.run_once()
                attempted = self.state()["attempt_at"]
                episode = self.state()["episode"]
                self.assertEqual(self.state()["stage"], "uncertain")
                self.assertFalse(self.state()["consumed"])
                self.assertFalse(self.state().get("reset_completed_at"))
                self.assertEqual(self.state()["error_code"], "already_redeemed" if code == "already_redeemed"
                                 else "result_uncertain")
                self.assertEqual(self.state()["consumption_outcome"], "unknown")
                self.assertEqual(self.calls, ["schedule:False", "reset"])
                self.make_controller(real_http=True)
                for _ in range(2):
                    self.now += timedelta(hours=2)
                    self.run_once()
                self.assertEqual(self.state()["episode"], episode)
                self.assertEqual(self.state()["attempt_at"], attempted)
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertNotIn("test", self.calls)
                self.assertNotIn("recover", self.calls)
                self.assertNotIn("schedule:True", self.calls)
                self.assertEqual(self.store.pending_events(), [])

    def test_real_consumption_http_has_one_attached_quota_read_and_immediate_sse_test(self):
        self.run_once()
        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/reset-quota"),
            ("POST", "/api/v1/admin/accounts/7/test")])
        self.assertTrue(all(key == ADMIN_TOKEN for _, _, key in self.server.requests))
        self.assertEqual(self.server.quota_reads, 1)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 1)
        self.assertEqual(self.state()["stage"], "recovered")

    def test_missing_error_rows_use_account_block_fallback_once_and_merge_recovery_event(self):
        self.db.raw.execute("DELETE FROM ops_error_logs")

        self.run_once()

        self.assertEqual(self.state()["evidence_source"], "account_rate_limit")
        self.assertTrue(self.state()["evidence_fingerprint"])
        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/reset-quota"),
            ("POST", "/api/v1/admin/accounts/7/test"),
        ])
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(self.state()["stage"], "recovered")
        self.assertEqual(len(self.store.pending_events()), 1)
        event = self.store.pending_events()[0]
        self.assertEqual(event["status"], "recovered")
        self.assertEqual(event["reset_credit"]["consumed"], True)
        self.assertEqual(event["reset_credit"]["completed_at"], self.state()["reset_completed_at"])
        self.assertIn(event["reset_credit"]["verification_method"], {"connection", "model"})

        self.make_controller(real_http=True)
        self.now += timedelta(hours=1)
        self.run_once()

        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(len(self.server.requests), 2)
        self.assertEqual(len(self.store.pending_events()), 1)

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

    def test_native_snapshot_is_observed_once_then_reused_after_restart_and_hourly_budget(self):
        expiries = [NOW + timedelta(days=3), NOW + timedelta(days=2), NOW + timedelta(days=2)]
        snapshot = native_credit_snapshot(NOW, count=3, expiries=expiries)
        for index, card in enumerate(snapshot["credits"]):
            card["id"] = f"fixture-card-{index}"
        self.use_native_credit_response(snapshot)

        self.run_once()

        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/quota/refresh")])
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 0)
        observation = self.native_observation()
        self.assertIsInstance(observation, dict)
        self.assertEqual(observation["version"], 1)
        self.assertEqual(observation["available_count"], 3)
        self.assertEqual(observation["expires_at"], sorted(value.isoformat() for value in expiries))
        self.assertEqual(observation["observed_at"], NOW.isoformat())
        self.assertEqual(observation["window_minutes"], 10080)
        self.assertEqual(set(observation), {"version", "observed_at", "available_count", "expires_at",
                                           "snapshot_sha256", "credential_fingerprint", "window_reset_at",
                                           "window_minutes", "limit_fingerprint"})
        self.assertTrue(observation["window_reset_at"])
        self.assertEqual(len(observation["snapshot_sha256"]), 64)
        self.assertEqual(len(observation["credential_fingerprint"]), 64)
        self.assertEqual(len(observation["limit_fingerprint"]), 64)
        serialized = json.dumps(observation)
        self.assertNotIn(ADMIN_TOKEN, serialized)
        self.assertNotIn("fixture-access-v1", serialized)
        self.assertNotIn("fixture-card-", serialized)

        self.make_controller(real_http=True)
        self.now += timedelta(seconds=3599)
        self.step()
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 0)
        self.assertEqual(len(self.server.requests), 1)

        self.now += timedelta(seconds=1)
        self.step()
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/quota/refresh"),
            ("POST", "/api/v1/admin/openai/accounts/7/reset-quota"),
            ("POST", "/api/v1/admin/accounts/7/test")])
        self.assertEqual(len(self.store.pending_events()), 1)
        self.assertEqual(self.store.pending_events()[0]["reset_credit"]["consumed"], True)
        receipt = self.state()["receipt"]
        self.assertEqual(receipt["http_status"], 200)
        self.assertEqual(receipt["business_code"], "success")
        self.assertEqual(receipt["windows_reset"], 1)
        self.assertTrue(receipt["cache_refreshed"])
        self.assertTrue(receipt["cache_persisted"])
        self.assertEqual(set(receipt) - {"http_status", "business_code", "windows_reset", "cache_refreshed",
                                         "cache_persisted", "account_state_recovered"}, set())

        self.make_controller(real_http=True)
        self.now += timedelta(hours=1)
        self.run_once()
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertEqual(len(self.server.requests), 3)
        self.assertEqual(len(self.store.pending_events()), 1)

    def test_native_snapshot_refresh_preserves_both_429_evidence_paths(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        for has_upstream_error in (True, False):
            with self.subTest(has_upstream_error=has_upstream_error):
                self.fresh_case()
                self.server.requests.clear()
                self.base_url = self.server.base_url
                self.make_controller(real_http=True)
                self.use_native_credit_response(snapshot)
                if not has_upstream_error:
                    self.db.raw.execute("DELETE FROM ops_error_logs")

                self.run_once()

                expected = "upstream_error" if has_upstream_error else "account_rate_limit"
                self.assertEqual(self.state()["evidence_source"], expected)
                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
                    ("POST", "/api/v1/admin/openai/accounts/7/quota/refresh")])

    def test_native_observation_requeries_after_snapshot_or_credential_change(self):
        first_snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        self.use_native_credit_response(first_snapshot)
        self.run_once()
        first = self.native_observation()
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 0)

        changed_snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=4)])
        self.native_credit_response = copy.deepcopy(changed_snapshot)
        self.native_credit_db_snapshot = copy.deepcopy(changed_snapshot)
        self.row["extra"]["codex_reset_credit_snapshot"] = copy.deepcopy(changed_snapshot)
        self.touch()
        self.now += timedelta(hours=1)
        self.step()
        second = self.native_observation()
        self.assertEqual(self.calls.count("query"), 2)
        self.assertEqual(self.calls.count("reset"), 0)
        self.assertNotEqual(first["snapshot_sha256"], second["snapshot_sha256"])

        self.row["credentials"]["access_token"] = "fixture-access-v2"
        self.touch()
        self.now += timedelta(hours=1)
        self.step()
        third = self.native_observation()
        self.assertEqual(self.calls.count("query"), 3)
        self.assertEqual(self.calls.count("reset"), 0)
        self.assertNotEqual(second["credential_fingerprint"], third["credential_fingerprint"])

        next_reset = NOW + timedelta(days=6)
        self.query_seven_reset_at = next_reset
        self.row["rate_limit_reset_at"] = next_reset.isoformat()
        self.row["extra"]["codex_7d_reset_at"] = next_reset.isoformat()
        self.touch()
        self.now += timedelta(hours=1)
        changed_quota = wham(self.now, five=self.query_five, seven=self.query_seven,
                             seven_reset_at=next_reset)
        self.store.commit(results={ACCOUNT_ID: quota_result(changed_quota, self.row, self.now)})
        self.step()
        fourth = self.native_observation()
        self.assertEqual(self.calls.count("query"), 4)
        self.assertEqual(self.calls.count("reset"), 0)
        self.assertNotEqual(third["window_reset_at"], fourth["window_reset_at"])
        self.assertNotEqual(third["limit_fingerprint"], fourth["limit_fingerprint"])
        self.assertEqual(len(self.server.requests), 4)

    def test_zero_or_expired_native_credit_does_not_repeat_refresh_on_each_step(self):
        cases = (
            ("zero", native_credit_snapshot(NOW, count=0, expiries=[])),
            ("expired", native_credit_snapshot(NOW, count=1, expiries=[NOW - timedelta(seconds=1)])),
        )
        for name, snapshot in cases:
            with self.subTest(name=name):
                self.fresh_case()
                self.server.requests.clear()
                self.base_url = self.server.base_url
                self.make_controller(real_http=True)
                self.use_native_credit_response(snapshot)
                self.run_once()
                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual(len(self.server.requests), 1)
                self.now += timedelta(seconds=30)
                self.step()
                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual(len(self.server.requests), 1)

    def test_native_observation_requires_matching_database_snapshot_and_cache_confirmation(self):
        good = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        mismatch = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=3)])
        cases = (("db_mismatch", mismatch, True), ("cache_unconfirmed", good, False))
        for name, database_snapshot, cache_persisted in cases:
            with self.subTest(name=name):
                self.fresh_case()
                self.server.requests.clear()
                self.base_url = self.server.base_url
                self.make_controller(real_http=True)
                self.use_native_credit_response(good, database_snapshot=database_snapshot,
                                                cache_persisted=cache_persisted)
                self.run_once()
                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual(self.state()["error_code"], "credit_snapshot_unconfirmed")
                self.assertIsNone(self.native_observation())
                self.assertEqual(len(self.server.requests), 1)
                self.now += timedelta(seconds=30)
                self.step()
                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual(len(self.server.requests), 1)

    def test_native_observation_write_failure_cannot_authorize_card_consumption(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        self.use_native_credit_response(snapshot)
        write = self.store._write
        failed = []

        def fail_observation_once(data):
            account = data.get("scheduler", {}).get(str(ACCOUNT_ID), {})
            if account.get("reset_credit_observation") and not failed:
                failed.append(True)
                raise OSError("fixture cannot persist credit observation")
            return write(data)

        with patch.object(self.store, "_write", side_effect=fail_observation_once):
            try:
                self.run_once()
            except OSError:
                pass

        self.assertEqual(failed, [True])
        self.assertEqual(self.calls.count("query"), 1)
        self.assertEqual(self.calls.count("reset"), 0)
        self.assertIsNone(self.native_observation())
        self.assertEqual(len(self.server.requests), 1)
        self.make_controller(real_http=True)
        self.now += timedelta(seconds=30)
        self.step()
        self.assertEqual(self.calls.count("reset"), 0)
        self.assertEqual(len(self.server.requests), 1)

    def test_corrupt_observation_cannot_fall_back_to_legacy_timestamp_snapshot(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        for field, bad_value in (("snapshot_sha256", "invalid"), ("observed_at", "invalid-time")):
            with self.subTest(field=field):
                self.fresh_case()
                self.server.requests.clear()
                self.base_url = self.server.base_url
                self.make_controller(real_http=True)
                self.use_native_credit_response(snapshot)
                self.run_once()
                state_path = Path(self.settings.usage_query_state_path)
                state_data = json.loads(state_path.read_text())
                account = state_data["scheduler"][str(ACCOUNT_ID)]
                account["reset_credit_observation"][field] = bad_value
                state_path.write_text(json.dumps(state_data))
                self.row["extra"]["codex_reset_credit_snapshot"] = {
                    **copy.deepcopy(snapshot), "fetched_at": (NOW - timedelta(hours=2)).isoformat()}

                self.now += timedelta(seconds=30)
                with self.assertRaises(ValueError):
                    self.run_once()

                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual(len(self.server.requests), 1)

    def test_refresh_timestamp_outside_read_interval_is_not_trusted(self):
        snapshot = native_credit_snapshot(NOW, count=1, expiries=[NOW + timedelta(days=2)])
        cases = (("future", int(NOW.timestamp()) + 2, "incomplete_quota"),
                 ("too_old", int(NOW.timestamp()) - 2, "credit_snapshot_unconfirmed"))
        for name, fetched_at, error_code in cases:
            with self.subTest(name=name):
                self.fresh_case()
                self.server.requests.clear()
                self.base_url = self.server.base_url
                self.make_controller(real_http=True)
                self.use_native_credit_response(snapshot)
                self.native_credit_fetched_at = fetched_at

                self.run_once()

                self.assertEqual(self.calls.count("query"), 1)
                self.assertEqual(self.calls.count("reset"), 0)
                self.assertEqual(self.state()["error_code"], error_code)
                self.assertIsNone(self.native_observation())
                self.assertFalse(self.state().get("consumed", False))
                self.assertFalse(self.state().get("reset_completed_at"))
                self.assertEqual(len(self.server.requests), 1)

    def test_confirmed_consumption_with_cache_warning_still_tests_immediately(self):
        self.request_results.append({"success": True, "consumed": True, "uncertain": False,
            "data": {"code": "reset", "windows_reset": 1, "cache_refreshed": False,
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
        task = self.state()
        self.assertEqual(task["receipt"], {"http_status": 200, "business_code": "reset", "windows_reset": 1,
                                            "cache_refreshed": False, "cache_persisted": False})
        self.assertEqual(task["reset_completed_at"], NOW.isoformat())
        self.make_controller(real_http=True)
        self.now += timedelta(hours=2)
        self.run_once()
        self.assertEqual(self.state()["receipt"], task["receipt"])
        self.assertEqual(self.state()["reset_completed_at"], task["reset_completed_at"])
        self.assertTrue(self.state()["consumed"])
        self.assertEqual(self.calls.count("reset"), 1)

    def test_confirmed_reset_with_malformed_nested_quota_is_only_reconciled_by_later_query(self):
        self.use_native_credit_response(native_credit_snapshot(NOW))
        self.run_once()
        self.assertEqual(self.calls, ["query"])
        malformed = {"success": True, "consumed": True, "uncertain": False, "error_code": "",
            "data": {"code": "reset", "windows_reset": 1, "fetched_at": int((NOW + timedelta(hours=1)).timestamp()),
                     "quota": {"rate_limit": {"primary_window": {"limit_window_seconds": "bad",
                         "used_percent": 20, "reset_at": int((NOW + timedelta(hours=6)).timestamp())},
                         "secondary_window": {}}, "fetched_at": int((NOW + timedelta(hours=1)).timestamp())},
                     "cache_persisted": True, "cache_refreshed": True}}
        self.request_results.append(malformed)
        self.now += timedelta(hours=1)
        self.run_once()

        consumed = self.state()
        self.assertTrue(consumed["consumed"])
        self.assertEqual(consumed["stage"], "confirming")
        self.assertTrue(consumed["reset_completed_at"])
        self.assertEqual(consumed["receipt"]["business_code"], "reset")
        self.assertEqual(self.calls, ["query", "schedule:False", "reset", "test"])
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])

        self.now += timedelta(hours=1)
        self.run_once()
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("query"), 2)
        self.assertEqual(self.calls.count("test"), 1)
        self.assertNotIn("recover", self.calls)
        self.assertNotIn("schedule:True", self.calls)
        self.assertTrue(self.state()["consumed"])
        self.assertEqual(self.state()["stage"], "confirming")
        self.assertEqual(self.store.snapshot()["recovery_history"], {})
        self.assertEqual(self.store.pending_events(), [])

    def test_refresh_http_429_records_status_without_retaining_response_body(self):
        self.row["extra"]["codex_reset_credit_snapshot"] = native_credit_snapshot(
            NOW, count=1, expiries=[NOW + timedelta(days=2)])
        self.http_status_overrides["query"] = (429, {
            "code": "fixture-card-secret", "message": "fixture-access-v1",
            "card_id": "fixture-card-123"})

        self.run_once()

        self.assertEqual([(method, path) for method, path, _key in self.server.requests], [
            ("POST", "/api/v1/admin/openai/accounts/7/quota/refresh")])
        self.assertEqual(self.calls.count("reset"), 0)
        audit = Path(self.settings.audit_path).read_text()
        self.assertIn('"http_status": 429', audit)
        self.assertNotIn("fixture-card-secret", audit)
        self.assertNotIn("fixture-access-v1", audit)
        self.assertNotIn("fixture-card-123", audit)
        self.assertIsNone(self.native_observation())


class ResetCreditDiagnosticTests(unittest.TestCase):
    def execute_receipt(self, data, *, status=200, envelope_code=0):
        class Response:
            def __enter__(response):
                return response

            def __exit__(response, *_args):
                return False

            def read(response, _limit):
                return json.dumps({"code": envelope_code, "data": data}).encode()

        response = Response()
        response.status = status
        return execute_credit_request("reset", 1, base_url="http://example.invalid",
            admin_token="fixture-admin-token", urlopen=lambda *_a, **_k: response)

    def test_native_reset_requires_a_positive_integer_window_count(self):
        for windows in (1, 2):
            with self.subTest(windows=windows):
                data = {"code": "reset", "windows_reset": windows, "fetched_at": int(NOW.timestamp()),
                        "quota": wham(NOW), "cache_refreshed": True, "cache_persisted": True}
                result = self.execute_receipt(data)
                self.assertTrue(result["success"])
                self.assertTrue(result["consumed"])
                self.assertFalse(result["uncertain"])
                self.assertEqual(result["error_code"], "")
                self.assertEqual(result["diagnostics"]["business_code"], "reset")
                self.assertEqual(result["diagnostics"]["windows_reset"], windows)

    def test_native_zero_window_outcomes_confirm_no_consumption(self):
        for code in ("no_credit", "nothing_to_reset"):
            with self.subTest(code=code):
                result = self.execute_receipt({"code": code, "windows_reset": 0})
                self.assertFalse(result["success"])
                self.assertFalse(result["consumed"])
                self.assertFalse(result["uncertain"])
                self.assertEqual(result["error_code"], code)
                self.assertEqual(result["diagnostics"], {"http_status": 200, "business_code": code,
                                                          "windows_reset": 0})

    def test_native_incomplete_unknown_or_conflicting_receipts_remain_uncertain(self):
        cases = [
            {"code": "reset"},
            *({"code": "reset", "windows_reset": value} for value in (0, -1, True, False, 1.0, "1", None)),
            *({"code": code, "windows_reset": value} for code in ("unrecognized_fixture",)
              for value in (0, 1)),
            *({"code": code, "windows_reset": value} for code in ("no_credit", "nothing_to_reset")
              for value in (1, True, None)),
        ]
        for data in cases:
            with self.subTest(data=data):
                result = self.execute_receipt(data)
                self.assertFalse(result["success"])
                self.assertFalse(result["consumed"])
                self.assertTrue(result["uncertain"])
                self.assertEqual(result["error_code"], "result_uncertain")

        for windows in (0, 1):
            with self.subTest(code="already_redeemed", windows=windows):
                result = self.execute_receipt({"code": "already_redeemed", "windows_reset": windows})
                self.assertFalse(result["success"])
                self.assertFalse(result["consumed"])
                self.assertTrue(result["uncertain"])
                self.assertEqual(result["error_code"], "already_redeemed")

    def test_native_receipt_cannot_override_a_failed_envelope_or_http_status(self):
        for options in ({"envelope_code": 1}, {"envelope_code": False}, {"status": 409}):
            with self.subTest(options=options):
                result = self.execute_receipt({"code": "reset", "windows_reset": 1}, **options)
                self.assertFalse(result.get("consumed", False))
                self.assertFalse(result["success"])
                self.assertTrue(result["uncertain"])

    def test_native_business_codes_are_preserved_without_extra_response_fields(self):
        for code in ("reset", "no_credit", "nothing_to_reset", "already_redeemed"):
            with self.subTest(code=code):
                self.assertEqual(receipt_diagnostics(200, {"code": code, "windows_reset": 0,
                    "message": "fixture-response-text", "card_id": "fixture-card-id"}),
                    {"http_status": 200, "business_code": code, "windows_reset": 0})

    def test_only_whitelisted_status_business_code_and_boolean_fields_are_retained(self):
        result = receipt_diagnostics(200, {
            "code": "OPENAI_QUOTA_REFRESHED", "windows_reset": 2,
            "cache_persisted": True, "cache_refreshed": False,
            "account_state_recovered": True, "message": "fixture-access-v1",
            "card_id": "fixture-card-123", "credential": "fixture-secret"})
        self.assertEqual(result, {"http_status": 200, "business_code": "OPENAI_QUOTA_REFRESHED",
                                  "windows_reset": 2, "cache_persisted": True,
                                  "cache_refreshed": False, "account_state_recovered": True})
        self.assertNotIn("fixture-access-v1", json.dumps(result))
        self.assertNotIn("fixture-card-123", json.dumps(result))
        self.assertNotIn("fixture-secret", json.dumps(result))

    def test_unknown_business_code_is_replaced_and_boolean_reset_count_is_omitted(self):
        result = receipt_diagnostics(429, {"code": "fixture-card-secret", "windows_reset": True,
                                           "cache_persisted": False, "detail": "fixture-access-v1"})
        self.assertEqual(result, {"http_status": 429, "business_code": "unrecognized",
                                  "cache_persisted": False})

    def test_reset_false_and_incomplete_business_codes_never_confirm_consumption(self):
        class Response:
            status = 200

            def __init__(self, payload):
                self.payload = json.dumps(payload).encode()

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self, _limit):
                return self.payload

        for business_code in (False, True, None, "", "OPENAI_CARD_REJECTED"):
            with self.subTest(business_code=business_code):
                payload = {"code": 0, "data": {"code": business_code, "windows_reset": 1}}
                result = execute_credit_request("reset", 1, base_url="http://example.invalid",
                    admin_token="fixture-admin-token", urlopen=lambda *_a, **_k: Response(payload))
                self.assertFalse(result["success"])
                self.assertFalse(result["consumed"])
                self.assertTrue(result["uncertain"])
                self.assertEqual(result["error_code"], "result_uncertain")

        missing = {"code": 0, "data": {"windows_reset": 1}}
        result = execute_credit_request("reset", 1, base_url="http://example.invalid",
            admin_token="fixture-admin-token", urlopen=lambda *_a, **_k: Response(missing))
        self.assertFalse(result["consumed"])
        confirmed = {"code": 0, "data": {"code": 0, "windows_reset": 1}}
        result = execute_credit_request("reset", 1, base_url="http://example.invalid",
            admin_token="fixture-admin-token", urlopen=lambda *_a, **_k: Response(confirmed))
        self.assertTrue(result["success"])
        self.assertTrue(result["consumed"])
        self.assertFalse(result["uncertain"])

    def test_structured_http_error_keeps_only_diagnostics_and_never_confirms_reset(self):
        payload = {"code": 0, "data": {"code": "OPENAI_CARD_EXPIRED", "windows_reset": 1,
                                        "cache_persisted": False, "message": "fixture-card-secret",
                                        "card_id": "fixture-card-123"}}
        error = urllib.error.HTTPError("http://example.invalid/reset", 409, "conflict", {},
                                       io.BytesIO(json.dumps(payload).encode()))
        def raise_http_error(*_args, **_kwargs):
            raise error
        result = execute_credit_request("reset", 1, base_url="http://example.invalid",
            admin_token="fixture-admin-token", urlopen=raise_http_error)
        self.assertFalse(result["success"])
        self.assertTrue(result["uncertain"])
        self.assertEqual(result["error_code"], "http_409")
        self.assertEqual(result["diagnostics"], {"http_status": 409, "business_code": "OPENAI_CARD_EXPIRED",
                                                  "windows_reset": 1, "cache_persisted": False})
        self.assertNotIn("fixture-card-secret", json.dumps(result["diagnostics"]))
        self.assertNotIn("fixture-card-123", json.dumps(result["diagnostics"]))


if __name__ == "__main__":
    unittest.main()
