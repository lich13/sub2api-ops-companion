from __future__ import annotations

import json
import tempfile
import threading
import time
import unittest
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urlsplit
from unittest.mock import patch

from app.account_locks import AccountLease
from app.oauth_monitor import OAuthMonitor, execute_sub2api_account_test
from app.oauth_queries import reset_not_before
from app.usage_query import execute_oauth_usage_query, oauth_quota_from_usage_data


NOW = datetime(2026, 10, 11, 12, 0, tzinfo=timezone.utc)


def row(account_id: int, *, reset_at: datetime | None = None) -> dict[str, object]:
    return {
        "id": account_id,
        "name": f"fixture-account-{account_id}",
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "credentials": {"plan_type": "plus", "access_token": f"fixture-token-{account_id}"},
        "extra": {},
        "updated_at": (NOW - timedelta(hours=2)).isoformat(),
        "rate_limited_at": (NOW - timedelta(minutes=5)).isoformat(),
        "rate_limit_reset_at": reset_at.isoformat() if reset_at else None,
        "temp_unschedulable_until": None,
        "temp_unschedulable_reason": "",
        "overload_until": None,
        "expires_at": None,
        "auto_pause_on_expired": False,
        "concurrency": 1,
        "load_factor": 0,
        "proxy_id": None,
    }


def usage(at: datetime, *, five: float = 0, seven: float = 0,
          five_reset: datetime | None = None,
          seven_reset: datetime | None = None,
          five_remaining: int | None = None) -> dict[str, object]:
    five_window: dict[str, object] = {
        "utilization": five,
        "resets_at": (five_reset or at + timedelta(hours=5)).isoformat(),
    }
    if five_remaining is not None:
        five_window["resets_at"] = "invalid-reset"
        five_window["remaining_seconds"] = five_remaining
    return {
        "five_hour": five_window,
        "seven_day": {
            "utilization": seven,
            "resets_at": (seven_reset or at + timedelta(days=7)).isoformat(),
        },
    }


def saved_result(account: dict[str, object], observed: datetime, data: dict[str, object]) -> dict[str, object]:
    return {
        "account_id": int(account["id"]),
        "template_type": "oauth",
        "success": True,
        "queried_at": observed.isoformat(),
        "oauth_quota": oauth_quota_from_usage_data(data, account, now=observed),
        "source": "sub2api_admin_usage",
    }


class FixtureDb:
    def __init__(self, rows: list[dict[str, object]]) -> None:
        self.rows = {int(item["id"]): item for item in rows}

    def fetch_all(self, _sql: str, _params: dict[str, object] | None = None) -> list[dict[str, object]]:
        return [dict(item) for item in self.rows.values()]

    def fetch_one(self, _sql: str, params: dict[str, object] | None = None) -> dict[str, object] | None:
        account_id = int((params or {}).get("account_id") or (params or {}).get("id") or 0)
        value = self.rows.get(account_id)
        return dict(value) if value else None


class RecoveryHTTP:
    """A local, isolated Sub2API-shaped endpoint used by dispatcher tests."""

    def __init__(self, rows: dict[int, dict[str, object]], clock: list[datetime]) -> None:
        self.rows = rows
        self.clock = clock
        self.usage_data: dict[int, dict[str, object]] = {}
        self.gets: list[int] = []
        self.posts: list[int] = []
        self.active_gets = 0
        self.max_active_gets = 0
        self.active_posts = 0
        self.max_active_posts = 0
        self.get_entered = {aid: threading.Event() for aid in rows}
        self.post_entered = {aid: threading.Event() for aid in rows}
        self.block_get = set()
        self.block_post = set()
        self.release_get = threading.Event()
        self.release_get.set()
        self.release_post = threading.Event()
        self.release_post.set()
        self.counter_lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def _account_id(self) -> int:
                pieces = urlsplit(self.path).path.strip("/").split("/")
                return int(pieces[4])

            def do_GET(self) -> None:
                aid = self._account_id()
                outer.gets.append(aid)
                outer.get_entered.setdefault(aid, threading.Event()).set()
                with outer.counter_lock:
                    outer.active_gets += 1
                    outer.max_active_gets = max(outer.max_active_gets, outer.active_gets)
                try:
                    if aid in outer.block_get:
                        outer.release_get.wait(timeout=10)
                    data = outer.usage_data.get(aid) or usage(outer.clock[0])
                    body = json.dumps({"data": data}).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                finally:
                    with outer.counter_lock:
                        outer.active_gets -= 1

            def do_POST(self) -> None:
                aid = self._account_id()
                outer.posts.append(aid)
                outer.post_entered.setdefault(aid, threading.Event()).set()
                with outer.counter_lock:
                    outer.active_posts += 1
                    outer.max_active_posts = max(outer.max_active_posts, outer.active_posts)
                try:
                    if aid in outer.block_post:
                        outer.release_post.wait(timeout=10)
                    if aid in outer.rows:
                        outer.rows[aid]["rate_limited_at"] = None
                        outer.rows[aid]["rate_limit_reset_at"] = None
                    body = b'data: {"type":"test_complete","success":true}\n\n'
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                finally:
                    with outer.counter_lock:
                        outer.active_posts -= 1

            def log_message(self, *_args: object) -> None:
                return None

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        self.release_get.set()
        self.release_post.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def block_get_account(self, account_id: int) -> None:
        self.block_get.add(account_id)
        self.release_get.clear()

    def block_post_account(self, account_id: int) -> None:
        self.block_post.add(account_id)
        self.release_post.clear()


class RecoveryDispatcherFixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.clock = [NOW]
        self.monitors: list[OAuthMonitor] = []
        self.servers: list[RecoveryHTTP] = []

    def tearDown(self) -> None:
        for server in self.servers:
            server.release_get.set()
            server.release_post.set()
        for monitor in self.monitors:
            monitor.close()
        for server in self.servers:
            server.close()

    def make_monitor(self, rows: list[dict[str, object]], *, usage_slots: int = 4,
                     test_slots: int = 2, state_name: str | None = None) -> tuple[OAuthMonitor, RecoveryHTTP, FixtureDb]:
        db = FixtureDb(rows)
        server = RecoveryHTTP(db.rows, self.clock)
        self.servers.append(server)
        state_path = self.root / (state_name or f"state-{len(self.monitors)}.json")
        settings = SimpleNamespace(
            usage_query_state_path=str(state_path),
            audit_path=str(self.root / f"audit-{len(self.monitors)}.jsonl"),
            oauth_recovery_monitor_enabled=True,
            oauth_daily_test_enabled=False,
            oauth_usage_refresh_concurrency=usage_slots,
            oauth_recovery_test_concurrency=test_slots,
            oauth_early_probe_batch_size=8,
            oauth_recovery_test_model_id="fixture-model",
            oauth_recovery_connection_account_ids=sorted(db.rows),
            oauth_recovery_model_account_ids=[],
        )
        monitor = OAuthMonitor(
            settings,
            db,
            base_url_provider=lambda: server.base_url,
            inventory_loader=lambda _db: [dict(value) for value in db.rows.values()],
            account_reader=lambda _db, aid: dict(db.rows.get(int(aid))) if int(aid) in db.rows else None,
            usage_runner=execute_oauth_usage_query,
            test_runner=execute_sub2api_account_test,
            recovery_runner=lambda *_args, **_kwargs: {"success": True, "method": "fixture"},
            clock=lambda: self.clock[0],
            inventory_observer=None,
        )
        monitor.store.save_admin_token("fixture-admin-key")
        monitor.auto_reset.run = lambda *_args, **_kwargs: None
        self.monitors.append(monitor)
        return monitor, server, db

    def seed(self, monitor: OAuthMonitor, results: dict[int, dict[str, object]],
             intents: dict[int, dict[str, object]] | None = None) -> None:
        scheduler = {
            aid: {
                "recovery_intent": intent,
            }
            for aid, intent in (intents or {}).items()
        }
        monitor.store.commit(results=results, scheduler_updates=scheduler)

    @staticmethod
    def wait_for(event: threading.Event, timeout: float = 4) -> None:
        if not event.wait(timeout):
            raise AssertionError("dispatcher worker did not reach the expected HTTP boundary")

    @staticmethod
    def wait_until(predicate, timeout: float = 4) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.01)
        raise AssertionError("dispatcher state did not settle before timeout")

    def intent(self, monitor: OAuthMonitor, aid: int) -> dict[str, object]:
        return dict(monitor.store.scheduler().get(aid, {}).get("recovery_intent") or {})

    def test_reset_not_before_exact_estimated_and_latest_boundary(self) -> None:
        account = row(1)
        exact = NOW + timedelta(seconds=17)
        exact_result = saved_result(account, NOW, usage(NOW, five=100, five_reset=exact))
        self.assertEqual(reset_not_before(account, exact_result), exact)

        estimated_result = saved_result(account, NOW, usage(NOW, five=100, five_remaining=17))
        self.assertEqual(reset_not_before(account, estimated_result), NOW + timedelta(seconds=77))

        mixed = saved_result(
            account,
            NOW,
            usage(NOW, five=100, seven=100, five_remaining=17,
                  seven_reset=NOW + timedelta(seconds=90)),
        )
        self.assertEqual(reset_not_before(account, mixed), NOW + timedelta(seconds=90))

    def test_tick_waits_for_exact_reset_then_follows_delayed_and_advanced_state(self) -> None:
        reset = NOW + timedelta(minutes=10)
        account = row(1, reset_at=reset)
        monitor, server, db = self.make_monitor([account])
        self.seed(monitor, {1: saved_result(account, NOW - timedelta(hours=2), usage(NOW))})

        monitor.dispatcher.tick(NOW)
        self.assertEqual(server.gets, [])

        db.rows[1]["rate_limit_reset_at"] = (NOW + timedelta(minutes=20)).isoformat()
        monitor.dispatcher.tick(NOW + timedelta(minutes=10))
        self.assertEqual(server.gets, [])
        self.assertEqual(self.intent(monitor, 1).get("waiting_reason"), "waiting_reset")

        db.rows[1]["rate_limit_reset_at"] = (NOW + timedelta(minutes=5)).isoformat()
        monitor.dispatcher.tick(NOW + timedelta(minutes=10))
        self.wait_for(server.get_entered[1])
        self.assertEqual(server.gets, [1])
        self.assertTrue(monitor.dispatcher.drain(timeout=4))

    def test_passive_available_quota_dispatches_without_sixty_second_delay(self) -> None:
        account = row(1, reset_at=NOW)
        monitor, server, _db = self.make_monitor([account])
        intent = {
            "fingerprint": "fixture-block",
            "due_at": NOW.isoformat(),
            "source": "server_exact",
            "window_keys": ["codex_5h", "codex_7d"],
            "kind": "rate_limit_block",
            "status": "pending",
            "attempt_count": 0,
        }
        self.seed(monitor, {1: saved_result(account, NOW, usage(NOW))}, {1: intent})

        monitor.dispatcher.tick(NOW - timedelta(seconds=1))
        self.assertEqual(server.gets, [])
        self.assertEqual(server.posts, [])

        started = time.monotonic()
        monitor.dispatcher.tick(NOW)
        self.wait_for(server.post_entered[1], timeout=2)
        self.assertLess(time.monotonic() - started, 2)
        self.assertEqual(server.gets, [])
        self.assertTrue(monitor.dispatcher.drain(timeout=4))

    def test_unknown_or_zero_reset_credit_skips_before_any_worker_http(self) -> None:
        for name, snapshot in (
            ("unknown", {}),
            ("zero", {"available_count": 0, "credits": []}),
        ):
            with self.subTest(name=name):
                account = row(1, reset_at=NOW + timedelta(hours=1))
                account["extra"] = {"codex_reset_credit_snapshot": snapshot}
                # FixtureDb returns the account row for any evidence lookup;
                # expose a bounded upstream event timestamp so depletion is
                # eligible without adding a separate database double.
                account["created_at"] = (NOW - timedelta(minutes=1)).isoformat()
                monitor, server, _db = self.make_monitor([account])
                monitor.settings.oauth_auto_reset_credit_enabled = True
                self.seed(
                    monitor,
                    {1: saved_result(account, NOW, usage(
                        NOW, five=100, seven=100, seven_reset=NOW + timedelta(days=5)
                    ))},
                )

                monitor.dispatcher.tick(NOW)

                self.assertTrue(monitor.dispatcher.drain(timeout=4))
                self.assertEqual(server.gets, [])
                self.assertEqual(server.posts, [])
                self.assertEqual(monitor.dispatcher.credits, {})
                admission = monitor.store.scheduler().get(1, {}).get("credit_admission") or {}
                self.assertEqual(admission.get("reason"), "no_credit" if name == "zero" else "credit_unconfirmed")

    def test_slow_quota_does_not_block_cached_account_and_repeated_ticks_do_not_duplicate(self) -> None:
        slow = row(1)
        fast = row(2)
        monitor, server, _db = self.make_monitor([slow, fast], usage_slots=2, test_slots=2)
        server.block_get_account(1)
        fast_intent = {
            "fingerprint": "fixture-fast",
            "due_at": (NOW - timedelta(minutes=1)).isoformat(),
            "source": "server_exact",
            "window_keys": ["codex_5h", "codex_7d"],
            "kind": "rate_limit_block",
            "status": "pending",
            "attempt_count": 0,
        }
        self.seed(
            monitor,
            {
                1: saved_result(slow, NOW - timedelta(hours=2), usage(NOW, five=100, five_reset=NOW)),
                2: saved_result(fast, NOW - timedelta(seconds=1), usage(NOW)),
            },
            {2: fast_intent},
        )

        monitor.dispatcher.tick(NOW)
        self.wait_for(server.get_entered[1])
        monitor.dispatcher.tick(NOW + timedelta(seconds=1))
        self.wait_for(server.post_entered[2])
        self.assertEqual(server.gets, [1])
        self.assertEqual(server.posts, [2])

        monitor.dispatcher.tick(NOW + timedelta(seconds=2))
        self.assertEqual(server.gets, [1])
        server.release_get.set()
        self.assertTrue(monitor.dispatcher.drain(timeout=6))
        monitor.dispatcher.tick(NOW + timedelta(seconds=3))
        self.assertEqual(server.gets.count(1), 1)
        self.assertEqual(server.posts.count(2), 1)

    def test_query_http_slots_are_global_across_account_workers(self) -> None:
        first, second = row(1), row(2)
        monitor, server, db = self.make_monitor([first, second], usage_slots=1, test_slots=1)
        server.block_get_account(1)
        server.block_get_account(2)
        visible = {1}
        monitor.dispatcher.state_loader = lambda _db, ids: [
            dict(db.rows[aid]) for aid in sorted(visible.intersection(int(value) for value in ids))
        ]
        results = {
            1: saved_result(first, NOW - timedelta(hours=2), usage(NOW, five=100, five_reset=NOW)),
            2: saved_result(second, NOW - timedelta(hours=2), usage(NOW, five=100, five_reset=NOW)),
        }
        self.seed(monitor, results)

        monitor.dispatcher.tick(NOW)
        self.wait_until(lambda: bool(server.gets))
        time.sleep(0.15)
        self.assertEqual(len(server.gets), 1)
        self.assertLessEqual(server.max_active_gets, 1)
        before_query = dict(monitor.store.scheduler().get(2, {}).get("quota_query") or {})
        visible.add(2)
        monitor.dispatcher.tick(NOW + timedelta(seconds=1))
        self.wait_until(lambda: self.intent(monitor, 2).get("waiting_reason") == "query_slots")
        self.assertEqual(server.gets, [1])
        self.assertLessEqual(server.max_active_gets, 1)
        after_query = dict(monitor.store.scheduler().get(2, {}).get("quota_query") or {})
        self.assertEqual(after_query.get("last_query_at"), before_query.get("last_query_at"))
        self.assertEqual(after_query.get("automatic_attempts"), before_query.get("automatic_attempts"))
        server.release_get.set()
        self.assertTrue(monitor.dispatcher.drain(timeout=6))
        monitor.dispatcher.tick(NOW + timedelta(seconds=3))
        self.wait_until(lambda: len(server.gets) >= 2)
        self.assertLessEqual(server.max_active_gets, 1)
        server.release_get.set()
        self.assertTrue(monitor.dispatcher.drain(timeout=6))

    def test_test_http_slots_are_global_across_account_workers(self) -> None:
        first, second = row(1), row(2)
        monitor, server, _db = self.make_monitor([first, second], usage_slots=2, test_slots=1)
        due_intent = {
            "due_at": (NOW - timedelta(minutes=1)).isoformat(),
            "source": "server_exact",
            "window_keys": ["codex_5h", "codex_7d"],
            "kind": "rate_limit_block",
            "status": "pending",
            "attempt_count": 0,
        }
        self.seed(
            monitor,
            {
                1: saved_result(first, NOW, usage(NOW)),
                2: saved_result(second, NOW, usage(NOW)),
            },
            {
                1: {**due_intent, "fingerprint": "fixture-first"},
                2: {**due_intent, "fingerprint": "fixture-second"},
            },
        )
        server.block_post_account(1)
        server.block_post_account(2)

        monitor.dispatcher.tick(NOW)
        self.wait_until(lambda: bool(server.posts))
        time.sleep(0.15)
        self.assertEqual(len(server.posts), 1)
        self.assertEqual(server.max_active_posts, 1)

        server.release_post.set()
        self.assertTrue(monitor.dispatcher.drain(timeout=6))
        monitor.dispatcher.tick(NOW + timedelta(seconds=3))
        self.wait_until(lambda: len(server.posts) >= 2)
        self.assertLessEqual(server.max_active_posts, 1)
        self.assertTrue(monitor.dispatcher.drain(timeout=6))

    def test_confirmed_receipt_resumes_after_final_commit_failure_without_retest(self) -> None:
        account = row(1)
        monitor, server, db = self.make_monitor([account])
        self.seed(monitor, {1: saved_result(account, NOW - timedelta(seconds=1), usage(NOW))})
        original_commit = monitor.store.commit
        failure = {"pending": True}

        def fail_final_commit(**kwargs: object) -> None:
            if failure["pending"] and kwargs.get("recovery_history"):
                failure["pending"] = False
                raise OSError("fixture final commit failure")
            original_commit(**kwargs)

        state_name = monitor.store.path.name
        with patch.object(monitor.store, "commit", side_effect=fail_final_commit):
            monitor.dispatcher.tick(NOW)
            self.wait_for(server.post_entered[1])
            self.assertTrue(monitor.dispatcher.drain(timeout=6))

        self.assertFalse(failure["pending"])
        failed_intent = self.intent(monitor, 1)
        self.assertNotEqual(failed_intent.get("status"), "recovered")
        self.assertTrue(failed_intent.get("verification_dispatched"))
        self.assertTrue(failed_intent.get("verification_receipt"))
        self.assertIsNone(db.rows[1]["rate_limited_at"])
        posts = list(server.posts)

        monitor.close()
        restarted, restarted_server, _db2 = self.make_monitor(
            [dict(db.rows[1])], state_name=state_name
        )
        restarted.dispatcher.tick(NOW + timedelta(seconds=3))
        self.assertTrue(restarted.dispatcher.drain(timeout=6))
        self.assertEqual(restarted_server.posts, [])
        self.assertEqual(server.posts, posts)
        self.assertEqual(self.intent(restarted, 1).get("status"), "recovered")
        self.assertEqual(len(restarted.store.snapshot()["recovery_history"]), 1)

    def test_method_generation_change_rejects_stale_recovery_result(self) -> None:
        account = row(1)
        monitor, server, _db = self.make_monitor([account])
        self.seed(monitor, {1: saved_result(account, NOW - timedelta(seconds=1), usage(NOW))})
        original_test = monitor._recovery_test

        def change_method(*args: object, **kwargs: object) -> dict[str, object]:
            result = original_test(*args, **kwargs)
            monitor.settings.oauth_recovery_connection_account_ids = []
            monitor.settings.oauth_recovery_model_account_ids = [1]

            def bump_generation(data: dict[str, object]) -> None:
                metadata = data["scheduler"].setdefault("1", {})
                metadata["recovery_method_generation"] = int(
                    metadata.get("recovery_method_generation", 0)
                ) + 1

            monitor.store.transaction(bump_generation)
            return result

        with patch.object(monitor, "_recovery_test", side_effect=change_method):
            monitor.dispatcher.tick(NOW)
            self.wait_for(server.post_entered[1])
            self.assertTrue(monitor.dispatcher.drain(timeout=6))

        monitor.dispatcher.tick(NOW + timedelta(seconds=1))
        self.assertEqual(server.posts, [1])
        current = self.intent(monitor, 1)
        self.assertEqual(current.get("status"), "blocked")
        self.assertEqual(current.get("last_error_code"), "verification_uncertain")
        self.assertFalse(current.get("verification_receipt"))

    def test_busy_account_does_not_overwrite_cancelled_method_selection(self) -> None:
        account = row(1)
        monitor, server, _db = self.make_monitor([account])
        intent = {
            "fingerprint": "fixture-busy",
            "due_at": (NOW - timedelta(minutes=1)).isoformat(),
            "source": "server_exact",
            "window_keys": ["codex_5h", "codex_7d"],
            "kind": "rate_limit_block",
            "status": "pending",
            "attempt_count": 0,
        }
        self.seed(monitor, {1: saved_result(account, NOW, usage(NOW))}, {1: intent})

        def cancel_selection(*_args: object, **_kwargs: object) -> bool:
            monitor.settings.oauth_recovery_connection_account_ids = []
            monitor.settings.oauth_recovery_model_account_ids = [1]

            def mutate(data: dict[str, object]) -> None:
                metadata = data["scheduler"].setdefault("1", {})
                metadata["recovery_method_generation"] = int(
                    metadata.get("recovery_method_generation", 0)
                ) + 1
                metadata["recovery_intent"]["status"] = "cancelled"

            monitor.store.transaction(mutate)
            return False

        with patch.object(AccountLease, "acquire", side_effect=cancel_selection):
            monitor._run_cycle(NOW, force=False, account_rows=[account])

        self.assertEqual(self.intent(monitor, 1).get("status"), "cancelled")
        self.assertEqual(server.gets, [])
        self.assertEqual(server.posts, [])

    def test_failed_verification_receipt_is_marked_uncertain_and_restart_does_not_replay(self) -> None:
        account = row(1)
        monitor, server, _db = self.make_monitor([account])
        self.seed(monitor, {1: saved_result(account, NOW - timedelta(seconds=1), usage(NOW))})
        original_write = monitor.store._write

        def fail_receipt(data: dict[str, object]) -> None:
            scheduler = data.get("scheduler") or {}
            intent = (scheduler.get("1") or {}).get("recovery_intent") or {}
            if "verification_receipt" in intent:
                raise OSError("fixture receipt persistence failure")
            original_write(data)

        with patch.object(monitor.store, "_write", side_effect=fail_receipt):
            monitor.dispatcher.tick(NOW)
            self.wait_for(server.post_entered[1])
            self.assertTrue(monitor.dispatcher.drain(timeout=6))
            monitor.dispatcher.tick(NOW + timedelta(seconds=1))

        current = self.intent(monitor, 1)
        self.assertEqual(current.get("status"), "blocked")
        self.assertEqual(current.get("last_error_code"), "verification_uncertain")
        posts = list(server.posts)

        state_name = monitor.store.path.name
        monitor.close()
        restarted, restarted_server, _db2 = self.make_monitor([row(1)], state_name=state_name)
        restarted.dispatcher.tick(NOW + timedelta(seconds=2))
        self.assertEqual(restarted_server.posts, [])
        self.assertEqual(server.posts, posts)

    def test_manual_intervention_cancels_dispatched_verification(self) -> None:
        account = row(1)
        monitor, server, db = self.make_monitor([account])
        self.seed(monitor, {1: saved_result(account, NOW - timedelta(seconds=1), usage(NOW))})
        server.block_post_account(1)

        monitor.dispatcher.tick(NOW)
        self.wait_for(server.post_entered[1])
        monitor.store.manual_control(db.rows[1], False, NOW + timedelta(seconds=1))
        server.release_post.set()
        self.assertTrue(monitor.dispatcher.drain(timeout=6))
        monitor.dispatcher.tick(NOW + timedelta(seconds=2))

        current = self.intent(monitor, 1)
        self.assertEqual(current.get("status"), "cancelled")
        self.assertEqual(server.posts, [1])


if __name__ == "__main__":
    unittest.main()
