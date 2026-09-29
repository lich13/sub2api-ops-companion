from __future__ import annotations

import asyncio
import json
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import nullcontext
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from app.bark import BarkNotifier
from app.config_service import ConfigService
from app.desktop_api import DesktopService, UsageActionRequest, account_dto
from app.oauth_monitor import OAuthMonitor, OAuthStateStore
from app.oauth_queries import OAuthQueryCoordinator
from app.settings import Settings
from app.usage_query import execute_oauth_usage_query, oauth_quota_from_usage_data


NOW = datetime(2026, 9, 29, 20, 0, tzinfo=timezone.utc)


def account(**changes):
    return {
        "id": 7,
        "name": "query-budget-test",
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "credentials": {"plan_type": "plus", "access_token": "fixture-token-v1"},
        "extra": {},
        **changes,
    }


def usage(now, *, five=20, seven=30, five_reset=None, seven_reset=None):
    return {
        "five_hour": {
            "utilization": five,
            "resets_at": (five_reset or now + timedelta(hours=5)).isoformat(),
        },
        "seven_day": {
            "utilization": seven,
            "resets_at": (seven_reset or now + timedelta(days=7)).isoformat(),
        },
    }


def saved_result(row, observed, data):
    return {
        "account_id": row["id"],
        "template_type": "oauth",
        "success": True,
        "queried_at": observed.isoformat(),
        "oauth_quota": oauth_quota_from_usage_data(data, row, now=observed),
        "source": "sub2api_admin_usage",
    }


class UsageServer:
    def __init__(self, clock):
        self.clock = clock
        self.requests = []
        self.posts = []
        self.status = 200
        self.data = None
        self.raw_body = None
        self.test_error_code = None
        self.on_request = None
        self.release = threading.Event()
        self.release.set()
        self.entered = threading.Event()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                outer.requests.append({
                    "path": self.path,
                    "api_key": self.headers.get("x-api-key"),
                    "authorization": self.headers.get("Authorization"),
                })
                if outer.on_request:
                    outer.on_request()
                outer.entered.set()
                outer.release.wait(timeout=10)
                body = outer.raw_body
                if body is None:
                    data = outer.data if outer.data is not None else usage(outer.clock())
                    body = json.dumps({"data": data}).encode()
                try:
                    self.send_response(outer.status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *_args):
                pass

            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                outer.posts.append({"path": self.path, "payload": payload})
                if self.path.endswith("/test"):
                    event = ({"type": "error", "code": outer.test_error_code, "message": "fixture auth failure"}
                             if outer.test_error_code else {"type": "test_complete", "success": True})
                    body = ("data: " + json.dumps(event) + "\n\n").encode()
                    content_type = "text/event-stream"
                else:
                    body = b'{"code":200,"message":"success"}'
                    content_type = "application/json"
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(
            target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True
        )
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class OAuthQueryFixture:
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.now = NOW
        self.row = account()
        self.server = UsageServer(lambda: self.now)
        self.addCleanup(self.server.close)
        self.reset_store()

    def reset_store(self, name="state.json"):
        self.path = self.root / name
        self.store = OAuthStateStore(str(self.path))
        self.coordinator = self.make_coordinator(self.store)

    def make_coordinator(self, store):
        return OAuthQueryCoordinator(
            store, execute_oauth_usage_query,
            lambda: self.server.base_url, lambda: self.now, str(self.root / "audit.jsonl"),
        )

    def query(self, *, source="automatic", row=None, **kwargs):
        return self.coordinator.query(
            row or self.row, "fixture-admin-key", source=source,
            reason="query-coordinator-test", **kwargs,
        )

    def query_state(self):
        return self.store.scheduler()[7]["quota_query"]

    def assert_skipped_without_request(self, *, source="automatic", error_code=None):
        count = len(self.server.requests)
        result = self.query(source=source)
        self.assertEqual(len(self.server.requests), count, "A skipped query must not emit HTTP")
        self.assertTrue(result.get("skipped"), result)
        self.assertFalse(result["success"])
        if error_code is not None:
            self.assertEqual(result["error_code"], error_code)
        return result


class OAuthQueryCoordinatorTests(OAuthQueryFixture, unittest.TestCase):
    def test_monitor_reads_normal_passive_snapshots_without_active_queries(self):
        self.row["extra"] = {
            "codex_usage_updated_at": (NOW - timedelta(hours=2)).isoformat(),
            "codex_5h_used_percent": 20,
            "codex_5h_reset_at": (NOW + timedelta(hours=3)).isoformat(),
            "codex_7d_used_percent": 30,
            "codex_7d_reset_at": (NOW + timedelta(days=6)).isoformat(),
        }
        settings = SimpleNamespace(
            usage_query_state_path=str(self.path), audit_path=str(self.root / "audit.jsonl"),
            oauth_recovery_monitor_enabled=True, oauth_daily_test_enabled=False,
            oauth_usage_refresh_concurrency=2, oauth_recovery_test_concurrency=1,
            oauth_early_probe_batch_size=8, oauth_recovery_test_model_id="fixture-model",
        )
        monitor = OAuthMonitor(
            settings, object(), base_url_provider=lambda: self.server.base_url,
            inventory_loader=lambda _db: [self.row],
            account_reader=lambda _db, _id: dict(self.row),
            usage_runner=execute_oauth_usage_query,
            test_runner=lambda *_a, **_kw: self.fail("Normal quota must not trigger a model test"),
            recovery_runner=lambda *_a, **_kw: self.fail("Normal quota must not trigger recovery"),
            clock=lambda: self.now,
        )
        monitor.store.save_admin_token("fixture-admin-key")
        for elapsed in (0, 1, 4, 24, 48):
            with self.subTest(elapsed_hours=elapsed):
                self.now = NOW + timedelta(hours=elapsed)
                monitor.run_once(now=self.now)
                self.assertEqual(self.server.requests, [])

    def test_depleted_window_waits_until_exact_reset_plus_sixty_seconds(self):
        reset = NOW + timedelta(minutes=30)
        self.store.commit(results={7: saved_result(
            self.row, NOW - timedelta(hours=2), usage(NOW, five=100, five_reset=reset),
        )})
        for moment in (NOW, reset, reset + timedelta(seconds=59)):
            with self.subTest(moment=moment):
                self.now = moment
                result = self.assert_skipped_without_request(error_code="waiting_reset")
                self.assertEqual(result["next_query_at"], (reset + timedelta(seconds=60)).isoformat())
        self.now = reset + timedelta(seconds=60)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 1)

    def test_all_depleted_windows_must_reach_the_latest_reset_plus_grace(self):
        earlier, later = NOW + timedelta(hours=1), NOW + timedelta(hours=2)
        self.store.commit(results={7: saved_result(
            self.row, NOW - timedelta(hours=2),
            usage(NOW, five=100, seven=100, five_reset=earlier, seven_reset=later),
        )})
        self.now = earlier + timedelta(seconds=60)
        result = self.assert_skipped_without_request(error_code="waiting_reset")
        self.assertEqual(result["next_query_at"], (later + timedelta(seconds=60)).isoformat())
        self.now = later + timedelta(seconds=60)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 1)

    def test_automatic_queries_have_a_minimum_one_hour_interval(self):
        self.assertTrue(self.query()["success"])
        self.now += timedelta(seconds=3599)
        self.assert_skipped_without_request(error_code="query_cooldown")
        self.now += timedelta(seconds=1)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 2)
        self.assertEqual(self.server.requests[0], {
            "path": "/api/v1/admin/accounts/7/usage?source=active&force=true",
            "api_key": "fixture-admin-key", "authorization": None,
        })

    def test_six_request_budget_is_rolling_across_midnight(self):
        for hour in range(6):
            self.now = NOW + timedelta(hours=hour)
            self.assertTrue(self.query()["success"])
        self.assertNotEqual(NOW.date(), self.now.date())
        self.assertEqual(len(self.server.requests), 6)
        for elapsed in (timedelta(hours=6), timedelta(hours=24, seconds=-1)):
            self.now = NOW + elapsed
            blocked = self.assert_skipped_without_request(error_code="query_budget")
            self.assertEqual(blocked["next_query_at"], (NOW + timedelta(hours=24)).isoformat())
        self.now = NOW + timedelta(hours=24)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 7)
        self.assertEqual(len(self.query_state()["automatic_attempts"]), 6)

    def test_failures_back_off_one_three_six_then_twelve_hours(self):
        self.server.status = 503
        expected_delays = (1, 3, 6, 12, 12)
        for index, hours in enumerate(expected_delays, 1):
            with self.subTest(failure=index):
                attempt_time = self.now
                result = self.query()
                self.assertFalse(result["success"])
                self.assertEqual(result["error_code"], "http_503")
                retry = attempt_time + timedelta(hours=hours)
                self.assertEqual(self.query_state()["failure_count"], index)
                self.assertEqual(self.query_state()["retry_at"], retry.isoformat())
                self.now = retry - timedelta(seconds=1)
                blocked = self.assert_skipped_without_request()
                self.assertEqual(blocked["next_query_at"], retry.isoformat())
                self.now = retry
        self.assertEqual(len(self.server.requests), 5)

    def test_restart_preserves_automatic_budget_and_cooldown(self):
        for hour in range(6):
            self.now = NOW + timedelta(hours=hour)
            self.assertTrue(self.query()["success"])
        persisted = self.path.read_bytes()
        self.store = OAuthStateStore(str(self.path))
        self.coordinator = self.make_coordinator(self.store)
        self.assertEqual(self.path.read_bytes(), persisted)
        self.now += timedelta(hours=1)
        self.assert_skipped_without_request(error_code="query_budget")
        self.now = NOW + timedelta(hours=24)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 7)

    def test_restart_preserves_failure_backoff(self):
        self.server.status = 503
        self.query()
        self.now += timedelta(hours=1)
        self.query()
        self.store = OAuthStateStore(str(self.path))
        self.coordinator = self.make_coordinator(self.store)
        self.now = NOW + timedelta(hours=4, seconds=-1)
        blocked = self.assert_skipped_without_request(error_code="query_backoff")
        self.assertEqual(blocked["next_query_at"], (NOW + timedelta(hours=4)).isoformat())
        self.assertEqual(len(self.server.requests), 2)

    def test_reservation_is_on_disk_before_http_and_timeout_consumes_attempt(self):
        seen = []
        self.server.on_request = lambda: seen.append(json.loads(self.path.read_text()))
        self.server.release.clear()
        try:
            result = self.query(timeout_seconds=2)
        finally:
            self.server.release.set()
        self.assertEqual(result["error_code"], "timeout")
        self.assertFalse(result["success"])
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(len(seen), 1)
        reserved = seen[0]["scheduler"]["7"]["quota_query"]
        self.assertEqual(reserved["automatic_attempts"], [NOW.isoformat()])
        self.assertEqual(reserved["last_query_at"], NOW.isoformat())
        self.assertEqual(reserved["last_source"], "automatic")
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])
        self.assertEqual(self.query_state()["failure_count"], 1)
        self.assert_skipped_without_request()

    def test_corrupt_state_fails_closed_for_automatic_and_manual_requests(self):
        for invalid in ("{not-json", "[]"):
            self.path.write_text(invalid)
            for source in ("automatic", "manual"):
                with self.subTest(invalid=invalid, source=source):
                    self.assert_skipped_without_request(source=source, error_code="query_state_unavailable")
        self.assertEqual(self.server.requests, [])

    def test_deleted_existing_state_fails_closed_for_all_requests(self):
        self.store.save_admin_token("fixture-admin-key")
        self.path.unlink()
        for source in ("automatic", "manual"):
            with self.subTest(source=source):
                self.assert_skipped_without_request(source=source, error_code="query_state_unavailable")
        self.assertFalse(self.path.exists())
        self.assertEqual(self.server.requests, [])

    def test_valid_json_with_corrupt_query_state_never_emits_http(self):
        invalid_documents = [
            {"scheduler": []},
            {"scheduler": {"7": []}},
            {"scheduler": {"7": {"quota_query": []}}},
        ]
        for query in (
            {"automatic_attempts": "not-a-list"},
            {"automatic_attempts": None},
            {"automatic_attempts": ["invalid-time"]},
            {"last_query_at": "invalid-time"},
            {"last_query_at": []},
            {"retry_at": "invalid-time"},
            {"retry_at": {}},
            {"failure_count": -1},
            {"failure_count": True},
            {"failure_count": 1.5},
        ):
            invalid_documents.append({"scheduler": {"7": {"quota_query": query}}})
        for document in invalid_documents:
            for source in ("automatic", "manual"):
                with self.subTest(document=document, source=source):
                    self.path.write_text(json.dumps(document))
                    self.assert_skipped_without_request(source=source, error_code="query_state_unavailable")

    def test_reservation_write_failure_prevents_http_for_all_requests(self):
        with patch.object(self.store, "_write", side_effect=PermissionError("fixture read-only state")):
            for source in ("automatic", "manual"):
                with self.subTest(source=source):
                    self.assert_skipped_without_request(source=source, error_code="query_state_unavailable")
        self.assertEqual(self.server.requests, [])

    def test_result_write_failure_keeps_reserved_attempt_after_restart(self):
        write = self.store._write
        writes = 0

        def fail_second_write(data):
            nonlocal writes
            writes += 1
            if writes == 2:
                raise OSError("fixture disk full")
            return write(data)

        with patch.object(self.store, "_write", side_effect=fail_second_write):
            result = self.query()
        self.assertEqual(result["error_code"], "query_state_unavailable")
        self.assertFalse(result["skipped"])
        self.assertEqual(len(self.server.requests), 1)
        self.store = OAuthStateStore(str(self.path))
        self.coordinator = self.make_coordinator(self.store)
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])
        self.assert_skipped_without_request(error_code="query_cooldown")

    def test_auth_errors_pause_across_restart_until_credentials_change(self):
        for status in (401, 402):
            with self.subTest(status=status):
                self.reset_store(f"auth-{status}.json")
                self.now = NOW
                before = len(self.server.requests)
                self.server.status = status
                self.assertEqual(self.query()["error_code"], f"http_{status}")
                self.store = OAuthStateStore(str(self.path))
                self.coordinator = self.make_coordinator(self.store)
                self.now += timedelta(days=2)
                self.assert_skipped_without_request(error_code="auth_paused")
                refreshed = {**self.row, "credentials": {
                    **self.row["credentials"], "access_token": "fixture-token-v2",
                }}
                self.server.status = 200
                self.assertTrue(self.query(row=refreshed)["success"])
                self.assertEqual(len(self.server.requests), before + 2)
                self.assertNotIn("auth_fingerprint", self.query_state())

    def test_manual_success_unpauses_auth_without_erasing_automatic_attempt(self):
        for status in (401, 402):
            with self.subTest(status=status):
                self.reset_store(f"manual-auth-{status}.json")
                self.now = NOW
                self.server.status = status
                self.assertFalse(self.query()["success"])
                attempts = self.query_state()["automatic_attempts"]
                self.now += timedelta(minutes=10)
                self.server.status = 200
                self.assertTrue(self.query(source="manual")["success"])
                self.assertEqual(self.query_state()["automatic_attempts"], attempts)
                self.assertNotIn("auth_fingerprint", self.query_state())
                self.assertEqual(self.query_state()["failure_count"], 0)
                self.assertEqual(self.query_state()["retry_at"], "")
                self.assert_skipped_without_request(error_code="query_cooldown")
                self.now += timedelta(hours=1)
                self.assertTrue(self.query()["success"])

    def test_manual_requests_bypass_but_neither_consume_nor_clear_automatic_budget(self):
        for hour in range(6):
            self.now = NOW + timedelta(hours=hour)
            self.assertTrue(self.query()["success"])
        attempts = self.query_state()["automatic_attempts"]
        for minutes in (10, 20):
            self.now = NOW + timedelta(hours=5, minutes=minutes)
            self.assertTrue(self.query(source="manual")["success"])
            self.assertEqual(self.query_state()["automatic_attempts"], attempts)
        self.assertEqual(len(self.server.requests), 8)
        self.now += timedelta(hours=1)
        self.assert_skipped_without_request(error_code="query_budget")
        self.now = NOW + timedelta(hours=24)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 9)

    def test_manual_query_refreshes_one_hour_cooldown(self):
        self.assertTrue(self.query()["success"])
        self.now += timedelta(minutes=10)
        self.assertTrue(self.query(source="manual")["success"])
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])
        self.now = NOW + timedelta(hours=1)
        blocked = self.assert_skipped_without_request(error_code="query_cooldown")
        self.assertEqual(blocked["next_query_at"], (NOW + timedelta(minutes=70)).isoformat())
        self.now = NOW + timedelta(minutes=70)
        self.assertTrue(self.query()["success"])
        self.assertEqual(len(self.server.requests), 3)

    def test_concurrent_automatic_and_manual_queries_coalesce_to_one_http_request(self):
        requested_at = time.monotonic()
        self.server.release.clear()
        with ThreadPoolExecutor(max_workers=8) as executor:
            owner = executor.submit(self.query, requested_at=requested_at)
            self.assertTrue(self.server.entered.wait(timeout=3))
            gate = threading.Barrier(8)

            def follower(index):
                gate.wait(timeout=3)
                return self.query(source="manual" if index % 2 else "automatic", requested_at=requested_at)

            followers = [executor.submit(follower, index) for index in range(7)]
            try:
                gate.wait(timeout=3)
            finally:
                self.server.release.set()
            results = [owner.result(timeout=5), *(future.result(timeout=5) for future in followers)]
        self.assertTrue(all(result["success"] for result in results), results)
        self.assertEqual(sum(bool(result.get("coalesced")) for result in results), 7)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])

    def test_full_quota_with_future_reset_preserves_backoff_until_available_response(self):
        self.server.status = 503
        for hour in (0, 1, 4):
            self.now = NOW + timedelta(hours=hour)
            self.assertFalse(self.query()["success"])
        self.assertEqual(self.query_state()["retry_at"], (NOW + timedelta(hours=10)).isoformat())
        self.now = NOW + timedelta(hours=5)
        reset = NOW + timedelta(hours=7)
        self.server.status = 200
        self.server.data = usage(self.now, five=100, five_reset=reset)
        self.assertTrue(self.query(source="manual")["success"])
        self.assertEqual(self.query_state()["failure_count"], 3)
        self.assertEqual(self.query_state()["retry_at"], (NOW + timedelta(hours=10)).isoformat())
        self.now = reset + timedelta(seconds=60)
        blocked = self.assert_skipped_without_request(error_code="query_backoff")
        self.assertEqual(blocked["next_query_at"], (NOW + timedelta(hours=10)).isoformat())
        self.now = NOW + timedelta(hours=10)
        self.server.data = None
        self.assertTrue(self.query()["success"])
        self.assertEqual(self.query_state()["failure_count"], 0)
        self.assertEqual(self.query_state()["retry_at"], "")
        self.assertEqual(len(self.server.requests), 5)

    def test_missing_required_window_counts_as_failure_and_preserves_good_cache(self):
        for missing in ("five_hour", "seven_day"):
            with self.subTest(missing=missing):
                self.reset_store(f"missing-{missing}.json")
                self.now = NOW
                previous = saved_result(self.row, NOW - timedelta(hours=2), usage(NOW))
                self.store.commit(results={7: previous})
                self.server.data = usage(NOW)
                del self.server.data[missing]
                result = self.query()
                self.assertFalse(result["success"])
                self.assertEqual(result["error_code"], "incomplete_quota")
                self.assertEqual(self.query_state()["failure_count"], 1)
                self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])
                self.assertEqual(self.store.result(7), previous)
                self.assert_skipped_without_request()
        self.assertEqual(len(self.server.requests), 2)

    def test_free_plan_requires_only_seven_day_window(self):
        self.row["credentials"]["plan_type"] = "free"
        self.server.data = {"seven_day": usage(NOW)["seven_day"]}
        self.assertTrue(self.query()["success"])
        self.assertEqual(self.query_state()["failure_count"], 0)
        self.assertEqual(len(self.server.requests), 1)

    def test_invalid_http_json_counts_as_failure_and_consumes_budget(self):
        self.server.raw_body = b"not-json"
        result = self.query()
        self.assertFalse(result["success"])
        self.assertEqual(self.query_state()["failure_count"], 1)
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])
        self.assertEqual(len(self.server.requests), 1)
        self.assert_skipped_without_request()


class AccountDatabase:
    def __init__(self, rows):
        self.rows = rows
        self.read = threading.Event()

    def fetch_one(self, _sql, params=None):
        self.read.set()
        account_id = (params or {}).get("id", (params or {}).get("account_id"))
        return next((dict(row) for row in self.rows if row["id"] == account_id), None)

    def fetch_all(self, sql, _params=None):
        if sql.lstrip().startswith("SELECT a.id, a.name, a.platform"):
            return [dict(row) for row in self.rows]
        return []

    def connection(self):
        return nullcontext(self)

    def transaction(self):
        return nullcontext()

    def execute(self, _sql, _params=None):
        return SimpleNamespace(fetchall=lambda: [])


class OAuthQueryLinkageTests(OAuthQueryFixture, unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.database = AccountDatabase([self.row])
        self.settings = Settings(
            database_url="postgresql://fixture:fixture@127.0.0.1/unused",
            base_path="", audit_path=str(self.root / "audit.jsonl"),
            usage_query_state_path=str(self.path), oauth_config_path=str(self.root / "oauth.json"),
            oauth_daily_test_enabled=False, bark_enabled=True,
            bark_device_key="fixture-device", bark_server_url=self.server.base_url,
        )
        self.monitor = OAuthMonitor(
            self.settings, self.database,
            base_url_provider=lambda: self.server.base_url,
            inventory_loader=lambda _db: [dict(row) for row in self.database.rows],
            account_reader=lambda db, account_id: db.fetch_one("fixture", {"id": account_id}),
            usage_runner=execute_oauth_usage_query,
            clock=lambda: self.now,
        )
        self.store = self.monitor.store
        self.coordinator = self.monitor.queries
        self.store.save_admin_token("fixture-admin-key")
        runtime = SimpleNamespace(
            db=self.database, settings=self.settings, oauth_monitor=self.monitor,
            oauth_base_url=lambda: self.server.base_url, key_fallback_controller=None,
        )
        self.service = DesktopService(runtime)

    async def asyncTearDown(self):
        await self.service.close()
        if self.service.quality._thread:
            await asyncio.to_thread(self.service.quality._thread.join, 2)

    async def single(self):
        payload = UsageActionRequest(
            action="query_usage", expected_version=account_dto(self.row, self.now, set())["version"],
        )
        return await asyncio.to_thread(self.service.usage_action, 7, payload, "fixture-admin-key")

    async def wait_for(self, event):
        self.assertTrue(await asyncio.to_thread(event.wait, 3), "Expected operation did not start")

    async def finish_batch(self):
        await asyncio.wait_for(self.service.actions.batch_task, 5)
        result = self.service.actions.batch_view()
        self.assertEqual(result["status"], "completed", result)
        self.assertEqual(result["completed"], 1)
        self.assertEqual(result["items"][0]["status"], "success", result)
        return result

    def enable_daily(self):
        self.settings.oauth_daily_test_enabled = True
        self.monitor.daily_schedule.tick(self.now)
        self.now = NOW + timedelta(hours=1)
        self.assertEqual(self.store.snapshot()["daily_test"]["next_run_at"], self.now.isoformat())

    def daily_record(self):
        batch = self.store.snapshot()["daily_test"]["batches"]["2026-09-30"]
        self.assertEqual(batch["status"], "completed", batch)
        return batch["accounts"]["7"]

    async def assert_no_bark_requests(self, events):
        self.assertEqual(events, [])
        self.assertEqual(self.store.pending_events(), [])
        notifier = BarkNotifier(self.settings)
        self.assertEqual(await asyncio.to_thread(notifier.notify_oauth_monitor_events, events), [])
        self.assertFalse(any(item["path"] == "/push" for item in self.server.posts))

    async def test_single_joins_running_batch_and_later_manual_query_runs_again(self):
        self.server.release.clear()
        await self.service.actions.start_batch("fixture-admin-key")
        await self.wait_for(self.server.entered)
        self.database.read.clear()
        single = asyncio.create_task(self.single())
        try:
            await self.wait_for(self.database.read)
        finally:
            self.server.release.set()
        response = await asyncio.wait_for(single, 5)
        await self.finish_batch()
        self.assertEqual(response["account"]["id"], 7)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.query_state().get("automatic_attempts", []), [])
        await self.single()
        self.assertEqual(len(self.server.requests), 2)
        await self.service.actions.start_batch("fixture-admin-key")
        await self.finish_batch()
        self.assertEqual(len(self.server.requests), 3)

    async def test_batch_joins_single_query_that_started_first(self):
        self.server.release.clear()
        single = asyncio.create_task(self.single())
        await self.wait_for(self.server.entered)
        try:
            await self.service.actions.start_batch("fixture-admin-key")
        finally:
            self.server.release.set()
        await asyncio.wait_for(single, 5)
        await self.finish_batch()
        self.assertEqual(len(self.server.requests), 1)

    async def test_single_joins_background_monitor_http_without_duplicate_request(self):
        self.server.release.clear()
        background = asyncio.create_task(asyncio.to_thread(self.monitor.run_once, self.now))
        await self.wait_for(self.server.entered)
        self.database.read.clear()
        single = asyncio.create_task(self.single())
        try:
            await self.wait_for(self.database.read)
        finally:
            self.server.release.set()
        response = await asyncio.wait_for(single, 5)
        await asyncio.wait_for(background, 5)
        self.assertEqual(response["account"]["id"], 7)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])
        await self.single()
        self.assertEqual(len(self.server.requests), 2)
        self.assertEqual(self.query_state()["automatic_attempts"], [NOW.isoformat()])

    async def test_background_cycle_does_not_duplicate_a_running_single_query(self):
        self.server.release.clear()
        single = asyncio.create_task(self.single())
        await self.wait_for(self.server.entered)
        try:
            await asyncio.wait_for(asyncio.to_thread(self.monitor.run_once, self.now), 3)
            self.assertEqual(len(self.server.requests), 1)
        finally:
            self.server.release.set()
        await asyncio.wait_for(single, 5)
        self.assertEqual(len(self.server.requests), 1)

    async def test_live_account_ineligible_after_inventory_never_emits_http(self):
        stale = dict(self.row)
        for changes in (
            {"schedulable": False}, {"status": "paused"},
            {"deleted_at": NOW.isoformat()}, {"type": "apikey"},
        ):
            with self.subTest(changes=changes):
                self.database.rows = [{**stale, **changes}]
                result = await asyncio.to_thread(self.query, row=stale)
                self.assertTrue(result.get("skipped"), result)
                self.assertIn(result["error_code"], ("account_ineligible", "account_changed"))
                self.assertEqual(self.server.requests, [])

    async def test_live_monitor_reserves_actual_request_time_after_inventory_delay(self):
        requested = NOW + timedelta(minutes=10)

        def delayed_inventory(_db):
            self.now = requested
            return [dict(self.row)]

        self.monitor.inventory_loader = delayed_inventory
        await asyncio.to_thread(self.monitor.run_once)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.query_state()["last_query_at"], requested.isoformat())
        self.assertEqual(self.query_state()["automatic_attempts"], [requested.isoformat()])
        self.assertEqual(self.store.result(7)["queried_at"], requested.isoformat())

    async def test_batch_cancellation_holds_locks_until_sent_request_finishes(self):
        self.server.release.clear()
        await self.service.actions.start_batch("fixture-admin-key")
        await self.wait_for(self.server.entered)
        task = self.service.actions.batch_task
        task.cancel()
        try:
            await asyncio.sleep(0.02)
            self.assertFalse(task.done())
            self.assertTrue(self.monitor._run_lock.locked())
            self.assertTrue(self.service.account_lock(7).locked())
            self.assertEqual(len(self.server.requests), 1)
        finally:
            self.server.release.set()
        await asyncio.wait_for(task, 5)
        self.assertFalse(self.monitor._run_lock.locked())
        self.assertFalse(self.service.account_lock(7).locked())
        await self.single()
        self.assertEqual(len(self.server.requests), 2)

    async def test_manual_batch_refreshes_shared_cooldown_without_consuming_budget(self):
        self.assertTrue((await asyncio.to_thread(self.query))["success"])
        attempts = self.query_state()["automatic_attempts"]
        self.now += timedelta(minutes=10)
        await self.service.actions.start_batch("fixture-admin-key")
        await self.finish_batch()
        self.assertEqual(self.query_state()["last_query_at"], self.now.isoformat())
        self.assertEqual(self.query_state()["last_source"], "manual")
        self.assertEqual(self.query_state()["automatic_attempts"], attempts)
        self.now = NOW + timedelta(hours=1)
        blocked = self.assert_skipped_without_request(error_code="query_cooldown")
        self.assertEqual(blocked["next_query_at"], (NOW + timedelta(minutes=70)).isoformat())
        self.now = NOW + timedelta(minutes=70)
        self.assertTrue((await asyncio.to_thread(self.query))["success"])
        self.assertEqual(len(self.server.requests), 3)

    async def test_daily_cannot_exceed_rolling_budget_and_does_not_notify_bark(self):
        for hour in range(-6, 0):
            self.now = NOW + timedelta(hours=hour)
            self.assertTrue((await asyncio.to_thread(self.query))["success"])
        self.now = NOW
        self.enable_daily()
        events = await asyncio.to_thread(self.monitor.run_once, self.now)
        record = self.daily_record()
        self.assertEqual(record["status"], "skipped", record)
        self.assertEqual(record["error_code"], "query_budget")
        self.assertEqual(len(self.server.requests), 6)
        self.assertEqual(self.server.posts, [])
        await self.assert_no_bark_requests(events)

    async def test_daily_waits_for_known_reset_and_does_not_notify_bark(self):
        self.store.commit(results={7: saved_result(
            self.row, NOW - timedelta(hours=2),
            usage(NOW, five=100, five_reset=NOW + timedelta(hours=2)),
        )})
        self.enable_daily()
        events = await asyncio.to_thread(self.monitor.run_once, self.now)
        record = self.daily_record()
        self.assertEqual(record["status"], "skipped", record)
        self.assertEqual(record["error_code"], "waiting_reset")
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.server.posts, [])
        await self.assert_no_bark_requests(events)

    async def test_daily_reuses_fresh_passive_quota_and_only_sends_model_test(self):
        due = NOW + timedelta(hours=1)
        self.row["extra"] = {
            "codex_usage_updated_at": (due - timedelta(minutes=10)).isoformat(),
            "codex_5h_used_percent": 20,
            "codex_5h_reset_at": (due + timedelta(hours=5)).isoformat(),
            "codex_7d_used_percent": 30,
            "codex_7d_reset_at": (due + timedelta(days=7)).isoformat(),
        }
        self.enable_daily()
        events = await asyncio.to_thread(self.monitor.run_once, self.now)
        self.assertEqual(self.daily_record()["status"], "success")
        self.assertEqual(self.server.requests, [])
        self.assertEqual([item["path"] for item in self.server.posts], ["/api/v1/admin/accounts/7/test"])
        self.assertEqual(self.query_state().get("automatic_attempts", []), [])
        await self.assert_no_bark_requests(events)

    async def test_daily_active_refresh_uses_automatic_budget_and_runs_only_once(self):
        self.settings.oauth_recovery_monitor_enabled = False
        self.enable_daily()
        events = await asyncio.to_thread(self.monitor.run_once, self.now)
        self.assertEqual(self.daily_record()["status"], "success")
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.query_state()["automatic_attempts"], [self.now.isoformat()])
        self.assertEqual([item["path"] for item in self.server.posts], ["/api/v1/admin/accounts/7/test"])
        await asyncio.to_thread(self.monitor.run_once, self.now + timedelta(seconds=1))
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(len(self.server.posts), 1)
        await self.assert_no_bark_requests(events)

    async def assert_daily_auth_failure_pauses_queries(self, code):
        self.settings.oauth_recovery_monitor_enabled = False
        self.server.test_error_code = code
        self.enable_daily()
        await asyncio.to_thread(self.monitor.run_once, self.now)
        self.assertEqual(self.daily_record()["status"], "failed")
        self.assertEqual(self.daily_record()["error_code"], code)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual([item["path"] for item in self.server.posts], ["/api/v1/admin/accounts/7/test"])
        self.now += timedelta(hours=1)
        self.assert_skipped_without_request(error_code="auth_paused")

    async def test_daily_model_http_401_pauses_later_automatic_queries(self):
        await self.assert_daily_auth_failure_pauses_queries("http_401")

    async def test_daily_model_http_402_pauses_later_automatic_queries(self):
        await self.assert_daily_auth_failure_pauses_queries("http_402")

    async def test_old_client_retired_probe_interval_is_accepted_but_not_saved_or_exposed(self):
        from app import main as runtime

        path = Path(self.settings.oauth_config_path)
        path.write_text(json.dumps({"oauth_7d_probe_interval_seconds": 60}))
        with patch.object(runtime, "settings", self.settings), patch.object(runtime, "oauth_monitor", None):
            config = ConfigService(runtime)
            before = config.snapshot("oauth")["oauth"]
            self.assertNotIn("oauth_7d_probe_interval_seconds", before)
            saved = await config.save("oauth", {
                "oauth_7d_probe_interval_seconds": 60,
                "oauth_daily_test_time": "06:15",
            }, "fixture-old-client", before["revision"])
            self.assertEqual(saved["oauth_daily_test_time"], "06:15")
            self.assertEqual(self.settings.oauth_daily_test_time, "06:15")
            self.assertNotIn("oauth_7d_probe_interval_seconds", saved)
            self.assertNotIn("oauth_7d_probe_interval_seconds", json.loads(path.read_text()))
            self.assertFalse(hasattr(self.settings, "oauth_7d_probe_interval_seconds"))
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.server.requests, [])


if __name__ == "__main__":
    unittest.main()
