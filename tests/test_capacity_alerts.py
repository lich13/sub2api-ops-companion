from __future__ import annotations

import asyncio
import copy
from contextlib import suppress
import json
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fastapi import HTTPException

from app.bark import BarkNotifier
from app.capacity_alerts import (
    MESSAGES,
    CapacityAlerts,
    mark_view,
    match_message,
)


class Clock:
    def __init__(self) -> None:
        self.value = datetime(2026, 10, 1, 12, 0, tzinfo=timezone.utc)

    def __call__(self) -> datetime:
        return self.value

    def advance(self, **kwargs: int) -> datetime:
        self.value += timedelta(**kwargs)
        return self.value


class CapacityDb:
    def __init__(self, rows: list[dict], accounts: dict[int, dict] | None = None) -> None:
        self.rows = rows
        self.accounts = accounts or {}
        self.fetch_all_calls: list[tuple[str, dict]] = []
        self.fetch_one_calls: list[tuple[str, dict]] = []
        self.initialized = threading.Event()

    def fetch_one(self, sql: str, params: dict | None = None) -> dict | None:
        params = params or {}
        self.fetch_one_calls.append((sql, copy.deepcopy(params)))
        if "coalesce(max(id)" in sql:
            self.initialized.set()
            return {"id": max((int(row["id"]) for row in self.rows), default=0)}
        account_id = params.get("id")
        return copy.deepcopy(self.accounts.get(account_id))

    def fetch_all(self, sql: str, params: dict) -> list[dict]:
        self.fetch_all_calls.append((sql, copy.deepcopy(params)))
        if "FROM usage_logs" in sql:
            return []
        if "created_at>=" in sql:
            after = int(params["after"])
            since = params["since"]
            rows = [
                row for row in self.rows
                if int(row["id"]) > after and _dt(row["created_at"]) >= since
            ]
        else:
            cursor = int(params["cursor"])
            rows = [row for row in self.rows if int(row["id"]) > cursor]
        return copy.deepcopy(sorted(rows, key=lambda row: int(row["id"]))[:200])


def _dt(value: object) -> datetime:
    if isinstance(value, datetime):
        return value
    return datetime.fromisoformat(str(value).replace("Z", "+00:00"))


def error_row(
    error_id: int,
    account_id: int = 1,
    created_at: datetime | None = None,
    *,
    message: str = MESSAGES[0],
    platform: str = "openai",
    account_type: str = "oauth",
    account_name: str | None = None,
    **changes: object,
) -> dict:
    row = {
        "id": error_id,
        "account_id": account_id,
        "created_at": (created_at or datetime(2026, 10, 1, 11, 59, tzinfo=timezone.utc)).isoformat(),
        "error_owner": "provider",
        "error_phase": "upstream",
        "requested_model": "gpt-5.6",
        "model": "gpt-5.6",
        "upstream_model": "gpt-5.6",
        "upstream_error_message": message,
        "error_message": None,
        "error_body": None,
        "upstream_error_detail": None,
        "upstream_status_code": 200,
        "account_name": account_name or f"Account {account_id}",
        "account_platform": platform,
        "account_type": account_type,
        "account_deleted_at": None,
    }
    row.update(changes)
    return row


class BarkCapture:
    def __init__(self, responses: list[tuple[int, dict]] | None = None) -> None:
        self.requests: list[dict] = []
        self.responses = list(responses or [])
        self.lock = threading.Lock()
        self.requested = threading.Event()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def _handler(self):
        capture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", 0))
                body = self.rfile.read(length)
                with capture.lock:
                    capture.requests.append(json.loads(body))
                    status, payload = capture.responses.pop(0) if capture.responses else (200, {"code": 200})
                    capture.requested.set()
                encoded = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, *_: object) -> None:
                return

        return Handler

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def wait_for_request(self, timeout: float = 3.0) -> bool:
        return self.requested.wait(timeout)


class CapacityAlertTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.clock = Clock()
        self.capture = BarkCapture()
        self.settings = SimpleNamespace(
            usage_query_state_path=str(Path(self.tmp.name) / "usage-state.json"),
            audit_path=str(Path(self.tmp.name) / "audit.jsonl"),
            bark_enabled=True,
            bark_config_valid=True,
            bark_device_key="isolated-device-key",
            bark_server_url=self.capture.url,
        )
        self.notifier = BarkNotifier(self.settings)
        self.accounts = {
            1: {"platform": "openai", "type": "oauth", "deleted_at": None},
            2: {"platform": "openai", "type": "oauth", "deleted_at": None},
            3: {"platform": "grok", "type": "oauth", "deleted_at": None},
        }

    def tearDown(self) -> None:
        self.capture.close()
        self.tmp.cleanup()

    def make_alerts(self, db: CapacityDb | None = None, notifier=None) -> tuple[CapacityAlerts, CapacityDb]:
        database = db or CapacityDb([], self.accounts)
        alerts = CapacityAlerts(self.settings, database, notifier or self.notifier, clock=self.clock)
        return alerts, database

    def test_match_requires_exact_provider_openai_oauth_error(self) -> None:
        plain = error_row(1, message="  OUR SERVERS ARE CURRENTLY OVERLOADED. Please try again later!!!  ")
        self.assertEqual(match_message(plain), MESSAGES[0])

        encoded = error_row(
            2,
            message=None,
            error_body=json.dumps({"error": {"message": "Selected model is at capacity. Please try a different model."}}),
            upstream_status_code=200,
        )
        self.assertEqual(match_message(encoded), MESSAGES[1])

        stream_error = error_row(
            11,
            message=None,
            upstream_error_detail={"error": {"message": MESSAGES[2]}},
        )
        self.assertEqual(match_message(stream_error), MESSAGES[2])
        self.assertEqual(match_message(error_row(
            12,
            message="  STREAM   DISCONNECTED BEFORE COMPLETION: concurrency limit exceeded for account, please retry later!!! ",
        )), MESSAGES[2])

        rejected = [
            error_row(3, platform="grok"),
            error_row(4, account_type="client_key"),
            error_row(5, account_deleted_at="2026-10-01T12:00:00+00:00"),
            error_row(6, error_owner="client"),
            error_row(7, error_phase="request"),
            error_row(10, message=None, error_body=MESSAGES[0]),
            error_row(8, message=None, error_body={"request": {"error": {"message": MESSAGES[0]}}}),
            error_row(9, message=None, error_body=json.dumps({"request": {"error": {"message": MESSAGES[1]}}})),
            error_row(13, message=None, error_body={"error": {"message": "stream disconnected before completion: concurrency limit exceeded for account"}}),
        ]
        for row in rejected:
            with self.subTest(row=row["id"]):
                self.assertIsNone(match_message(row))

    def test_first_watermark_has_no_history_and_each_new_event_creates_one_clue(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(1, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)

        alerts.poll()
        self.assertEqual(alerts.store.snapshot()["cursor"], 1)
        self.assertEqual(alerts.store.snapshot()["pending"], {})
        self.assertEqual(self.capture.requests, [])

        self.clock.advance(seconds=1)
        rows.extend([
            error_row(2, account_id=1, created_at=self.clock()),
            error_row(3, account_id=2, created_at=self.clock()),
        ])
        alerts.poll()
        alerts.deliver_due()
        self.assertEqual(set(alerts.store.snapshot()["detection_events"]), {"error:2", "error:3"})
        self.assertEqual(alerts.store.snapshot()["pending"], {})
        self.assertEqual(self.capture.requests, [])

        alerts.poll()
        alerts.deliver_due()
        self.assertEqual(set(alerts.store.snapshot()["detection_events"]), {"error:2", "error:3"})
        self.assertEqual(self.capture.requests, [])

    def test_pagination_replay_delay_and_restart_continue_from_persisted_cursor(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(2, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)
        alerts.poll()

        # A delayed insert with an ID below the cursor is picked up by the replay window.
        self.clock.advance(seconds=1)
        rows.append(error_row(1, account_id=1, created_at=self.clock()))
        alerts.poll()
        self.assertIn("error:1", alerts.store.snapshot()["detection_events"])

        # More than one page drains fully without calling Bark during collection.
        rows.extend(error_row(i, account_id=1, created_at=self.clock()) for i in range(3, 404))
        alerts.poll()
        state = alerts.store.snapshot()
        self.assertEqual(len(state["detection_events"]), 402)
        self.assertEqual(state["pending"], {})
        cursor_calls = [call for call in db.fetch_all_calls if "cursor" in call[1]]
        self.assertGreaterEqual(len(cursor_calls), 3)
        self.assertEqual(self.capture.requests, [])

        restarted = CapacityAlerts(self.settings, db, self.notifier, clock=self.clock)
        restarted.deliver_one("1")
        self.assertEqual(self.capture.requests, [])
        self.assertIn("error:1", restarted.store.snapshot()["detection_events"])

        self.clock.advance(seconds=1)
        rows.append(error_row(404, account_id=2, created_at=self.clock()))
        restarted.poll()
        restarted.deliver_one("404")
        self.assertEqual(self.capture.requests, [])
        self.assertIn("error:404", restarted.store.snapshot()["detection_events"])
        self.assertEqual(restarted.store.snapshot()["cursor"], 404)

    def test_raw_clues_never_enter_bark_retry_schedule(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(1, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)
        alerts.poll()
        self.clock.advance(seconds=1)
        rows.append(error_row(2, account_id=1, created_at=self.clock()))
        alerts.poll()

        for delay in (5, 30, 120, 600):
            alerts.deliver_due()
            self.assertEqual(alerts.store.snapshot()["pending"], {})
            self.assertIn("error:2", alerts.store.snapshot()["detection_events"])
            self.clock.advance(seconds=delay)
        alerts.deliver_due()
        self.assertEqual(self.capture.requests, [])

    def test_save_failure_does_not_advance_cursor_or_claim_delivery(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        db = CapacityDb([error_row(1, created_at=old)], self.accounts)
        alerts, _ = self.make_alerts(db)
        with patch("app.capacity_alerts.write_json", side_effect=OSError("state-save-failed")):
            with self.assertRaises(OSError):
                alerts.poll()
        self.assertFalse(alerts.store.path.exists())
        self.assertIsNone(alerts.store.snapshot()["cursor"])
        restarted_after_failed_save = CapacityAlerts(self.settings, db, self.notifier, clock=self.clock)
        with self.assertRaises(OSError):
            restarted_after_failed_save.store.snapshot()
        self.assertEqual(self.capture.requests, [])

        # A failed clue write cannot advance the cursor or lose the evidence.
        second_settings = SimpleNamespace(**vars(self.settings))
        second_settings.usage_query_state_path = str(Path(self.tmp.name) / "second" / "usage-state.json")
        db2 = CapacityDb([error_row(1, created_at=old)], self.accounts)
        alerts = CapacityAlerts(second_settings, db2, self.notifier, clock=self.clock)
        alerts.poll()
        self.clock.advance(seconds=1)
        alerts.db.rows.append(error_row(2, account_id=1, created_at=self.clock()))
        with patch("app.capacity_alerts.write_json", side_effect=OSError("state-save-failed")):
            with self.assertRaises(OSError):
                alerts.poll()
        self.assertEqual(alerts.store.snapshot()["cursor"], 1)
        self.assertEqual(alerts.store.snapshot()["detection_events"], {})
        alerts.poll()
        self.assertEqual(alerts.store.snapshot()["cursor"], 2)
        self.assertIn("error:2", alerts.store.snapshot()["detection_events"])
        self.assertEqual(self.capture.requests, [])

    def test_mark_suppresses_new_clues_but_preserves_other_account(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(1, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)
        alerts.poll()
        expected = mark_view(1)["version"]
        alerts.store.set_mark(1, True, expected, self.clock())
        self.clock.advance(seconds=1)
        rows.extend([
            error_row(2, account_id=1, created_at=self.clock()),
            error_row(3, account_id=2, created_at=self.clock()),
        ])
        alerts.poll()
        alerts.deliver_one("2")
        state = alerts.store.snapshot()
        self.assertNotIn("error:2", state["detection_events"])
        self.assertIn("error:3", state["detection_events"])
        self.assertEqual(state["notifications"]["2"]["reason"], "degradation_mark")
        self.assertEqual(self.capture.requests, [])

    def test_mark_clears_legacy_pending_and_retry_but_preserves_other_account(self) -> None:
        alerts, _ = self.make_alerts()
        alerts.poll()
        with alerts.store.transaction() as data:
            for aid in (1, 2):
                data["pending"][str(aid)] = {
                    "id": aid, "account_id": aid, "account_name": "fixture-account",
                    "requested_model": "fixture-model", "upstream_model": "fixture-model", "message": MESSAGES[0],
                    "created_at": self.clock().isoformat(), "next_at": self.clock().isoformat(), "attempts": 1,
                }
        alerts.store.set_mark(1, True, mark_view(1)["version"], self.clock())
        state = alerts.store.snapshot()
        self.assertNotIn("1", state["pending"])
        self.assertIn("2", state["pending"])
        self.assertEqual(state["notifications"]["1"]["reason"], "degradation_mark")
        self.assertEqual(self.capture.requests, [])

    def test_mark_is_scoped_to_id_not_parent_or_same_name_account(self) -> None:
        self.accounts[10] = {
            "platform": "openai",
            "type": "oauth",
            "deleted_at": None,
            "parent_account_id": 1,
        }
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(1, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)
        alerts.poll()
        alerts.store.set_mark(1, True, mark_view(1)["version"], self.clock())
        self.clock.advance(seconds=1)
        rows.extend([
            error_row(2, account_id=1, account_name="Shared name", created_at=self.clock()),
            error_row(3, account_id=10, account_name="Shared name", created_at=self.clock()),
        ])
        alerts.poll()

        state = alerts.store.snapshot()
        self.assertNotIn("error:2", state["detection_events"])
        self.assertIn("error:3", state["detection_events"])
        self.assertEqual(set(state["marks"]), {"1"})

    def test_collect_loop_persists_one_new_clue_within_three_seconds_without_bark(self) -> None:
        async def scenario() -> None:
            old = self.clock() - timedelta(minutes=1)
            rows = [error_row(1, created_at=old)]
            db = CapacityDb(rows, self.accounts)
            alerts, _ = self.make_alerts(db)
            collector = asyncio.create_task(alerts.collect_loop())
            delivery = asyncio.create_task(alerts.delivery_loop())
            try:
                self.assertTrue(await asyncio.to_thread(db.initialized.wait, 1.0))
                self.clock.advance(seconds=1)
                rows.append(error_row(2, account_id=1, created_at=self.clock()))
                inserted_at = time.monotonic()
                while "error:2" not in alerts.store.snapshot()["detection_events"] and time.monotonic() - inserted_at < 3:
                    await asyncio.sleep(.05)
                elapsed = time.monotonic() - inserted_at
                self.assertLessEqual(elapsed, 3.0)
                self.assertIn("error:2", alerts.store.snapshot()["detection_events"])
                self.assertEqual(self.capture.requests, [])
                self.assertIn("2", alerts.store.snapshot()["seen"])
                await asyncio.sleep(0.25)
                self.assertEqual(self.capture.requests, [])
            finally:
                collector.cancel()
                delivery.cancel()
                with suppress(asyncio.CancelledError):
                    await collector
                with suppress(asyncio.CancelledError):
                    await delivery

        asyncio.run(scenario())

    def test_unmark_only_allows_events_after_change_and_mark_survives_restart(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(1, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)
        alerts.poll()

        marked = alerts.store.set_mark(1, True, mark_view(1)["version"], self.clock())
        self.clock.advance(seconds=1)
        alerts.store.set_mark(1, False, marked["version"], self.clock())
        changed_at = self.clock()
        rows.extend([
            error_row(2, account_id=1, created_at=changed_at - timedelta(seconds=1)),
            error_row(3, account_id=1, created_at=changed_at + timedelta(seconds=1)),
        ])
        self.clock.advance(seconds=1)
        alerts.poll()
        self.assertNotIn("error:2", alerts.store.snapshot()["detection_events"])
        self.assertIn("error:3", alerts.store.snapshot()["detection_events"])

        restarted = CapacityAlerts(self.settings, db, self.notifier, clock=self.clock)
        rows.append(error_row(4, account_id=1, created_at=changed_at - timedelta(seconds=1)))
        restarted.poll()
        self.assertNotIn("error:4", restarted.store.snapshot()["detection_events"])
        restarted.deliver_due()
        self.assertEqual(self.capture.requests, [])

        with self.assertRaises(HTTPException) as raised:
            restarted.store.set_mark(1, True, "0" * 64, self.clock())
        self.assertEqual(raised.exception.status_code, 409)

    def test_mark_and_account_guard_serialize_per_account(self) -> None:
        old = self.clock() - timedelta(minutes=1)
        rows = [error_row(1, created_at=old)]
        db = CapacityDb(rows, self.accounts)
        alerts, _ = self.make_alerts(db)
        alerts.poll()
        self.clock.advance(seconds=1)
        rows.append(error_row(2, account_id=1, created_at=self.clock()))
        alerts.poll()
        expected = mark_view(1)["version"]

        executor = ThreadPoolExecutor(max_workers=2)
        mark_started, entered, release = threading.Event(), threading.Event(), threading.Event()

        def guarded_operation():
            with alerts.store.account_guard(1):
                entered.set()
                release.wait(timeout=2)

        try:
            guarded = executor.submit(guarded_operation)
            self.assertTrue(entered.wait(timeout=2))

            def mark() -> dict:
                mark_started.set()
                return alerts.store.set_mark(1, True, expected, self.clock())

            marking = executor.submit(mark)
            self.assertTrue(mark_started.wait(timeout=2))
            time.sleep(0.05)
            self.assertFalse(marking.done())
            release.set()
            self.assertIsInstance(guarded.result(timeout=2), type(None))
            self.assertTrue(marking.result(timeout=2)["marked"])
        finally:
            release.set()
            executor.shutdown(wait=True)
        self.assertEqual(self.capture.requests, [])
        self.assertEqual(alerts.store.snapshot()["pending"], {})


if __name__ == "__main__":
    unittest.main()
