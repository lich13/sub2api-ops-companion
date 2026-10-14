#!/usr/bin/env python3
"""Compare fixture-only Companion writes and operation recovery.

Examples:
  .venv/bin/python scripts/server_mutation_contract.py
  .venv/bin/python scripts/server_mutation_contract.py --command-json '["server/target/debug/sub2ops-server", "serve", "--host", "{host}", "--port", "{port}"]'

The Python mode extracts the fixed app from 6be7c291ed38dcc337cb88f693639040a3463899.
Command mode can run the existing Rust binary. Both modes use a runner-owned
temporary PostgreSQL cluster, loopback-only upstream mock, and synthetic rows.
The operation batch cases hold a synthetic upstream SSE response to exercise
idempotency, cancellation, and restart after an unknown in-flight request.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
from typing import Any, Callable


ROOT = Path(__file__).resolve().parents[1]
REFERENCE_SHA = "6be7c291ed38dcc337cb88f693639040a3463899"
ACCOUNT_IDS = (4001, 4002, 4003, 4004)
PREFIX = "/api/desktop/v1"

sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts"))
try:
    import server_http_contract as http
except ImportError as exc:  # pragma: no cover - reported by the command result
    http = None  # type: ignore[assignment]
    IMPORT_ERROR = type(exc).__name__
else:
    IMPORT_ERROR = ""


class MutationFailure(RuntimeError):
    def __init__(self, expected: object, actual: object) -> None:
        self.expected = expected
        self.actual = actual
        super().__init__("contract difference")


def check(condition: bool, expected: object, actual: object) -> None:
    if not condition:
        raise MutationFailure(expected, actual)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_result(result: Any) -> Any:
    """Keep result evidence to synthetic statuses, counts, and scalar state."""
    if result is None or isinstance(result, (bool, int, float)):
        return result
    if isinstance(result, str):
        return result[:160]
    if isinstance(result, list):
        return [safe_result(item) for item in result[:20]]
    if isinstance(result, dict):
        return {str(key): safe_result(value) for key, value in list(result.items())[:40]
                if key not in {"detail", "error", "message", "reason", "requested"}}
    return type(result).__name__


class MutationUpstream:
    """Loopback Sub2API mock with observable writes and holdable test SSE."""

    def __init__(self, database_url: str) -> None:
        self.database_url = database_url
        self.lock = threading.Lock()
        self.calls: Counter[str] = Counter()
        self.unclassified = 0
        self.fixture_errors = 0
        self.hold_accounts: set[int] = set()
        self.started: dict[int, threading.Event] = {}
        self.release: dict[int, threading.Event] = {}
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args) -> None:
                pass

            def body(self) -> dict[str, Any]:
                length = int(self.headers.get("Content-Length", "0") or "0")
                raw = self.rfile.read(length) if length else b""
                try:
                    value = json.loads(raw or b"{}")
                except (UnicodeError, ValueError):
                    return {}
                return value if isinstance(value, dict) else {}

            def count(self, name: str) -> None:
                with fixture.lock:
                    fixture.calls[name] += 1

            def reply(self, status: int, value: object) -> None:
                raw = json.dumps(value, ensure_ascii=False).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.send_header("Connection", "close")
                self.end_headers()
                try:
                    self.wfile.write(raw)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def mutate_account(self, statement: str, params: tuple[object, ...]) -> bool:
                try:
                    import psycopg

                    with psycopg.connect(fixture.database_url, autocommit=True) as connection:
                        cursor = connection.execute(statement, params)
                        return cursor.rowcount > 0
                except Exception:
                    with fixture.lock:
                        fixture.fixture_errors += 1
                    return False

            def do_GET(self) -> None:
                path = urlsplit(self.path).path
                if path == "/api/v1/admin/groups/all":
                    self.count("auth")
                    valid = self.headers.get("x-api-key") == http.ADMIN_KEY
                    self.reply(200 if valid else 401, {"code": 0 if valid else 401, "data": []})
                    return
                if path == "/api/v1/admin/system/version":
                    self.count("admin_version")
                    self.reply(200, {"code": 0, "data": {"version": "0.2.9"}})
                    return
                with fixture.lock:
                    fixture.unclassified += 1
                self.reply(404, {"code": 404, "message": "fixture endpoint not found"})

            def do_POST(self) -> None:
                path = urlsplit(self.path).path
                payload = self.body()
                test_match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)/test", path)
                if test_match:
                    account_id = int(test_match.group(1))
                    self.count(f"sse:{account_id}")
                    with fixture.lock:
                        start = fixture.started.setdefault(account_id, threading.Event())
                        release = fixture.release.setdefault(account_id, threading.Event())
                        held = account_id in fixture.hold_accounts
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    model = str(payload.get("model_id") or "fixture-native-model")
                    events = [
                        {"type": "test_start", "model": model},
                        {"type": "test_complete", "success": True},
                    ]
                    try:
                        self.wfile.write(("data: " + json.dumps(events[0]) + "\n\n").encode("utf-8"))
                        self.wfile.flush()
                        start.set()
                        if held:
                            release.wait(timeout=35)
                        self.wfile.write(("data: " + json.dumps(events[1]) + "\n\n").encode("utf-8"))
                        self.wfile.flush()
                    except (BrokenPipeError, ConnectionResetError):
                        pass
                    return

                schedule_match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)/schedulable", path)
                if schedule_match:
                    account_id = int(schedule_match.group(1))
                    enabled = payload.get("schedulable")
                    self.count(f"schedulable:{account_id}")
                    updated = type(enabled) is bool and self.mutate_account(
                        "UPDATE accounts SET schedulable=%s, updated_at=now() WHERE id=%s",
                        (enabled, account_id),
                    )
                    self.reply(200 if updated else 404, {
                        "code": 0 if updated else 404,
                        "data": {"id": account_id, "schedulable": enabled} if updated else {},
                    })
                    return

                if path.startswith("/api/v1/admin/"):
                    with fixture.lock:
                        fixture.unclassified += 1
                    self.reply(200, {"code": 0, "data": {}})
                    return
                with fixture.lock:
                    fixture.unclassified += 1
                self.reply(404, {"code": 404, "message": "fixture endpoint not found"})

            def do_PUT(self) -> None:
                path = urlsplit(self.path).path
                payload = self.body()
                match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)", path)
                if match and type(payload.get("priority")) is int:
                    account_id = int(match.group(1))
                    self.count(f"account_update:{account_id}")
                    updated = self.mutate_account(
                        "UPDATE accounts SET priority=%s, updated_at=now() WHERE id=%s",
                        (payload["priority"], account_id),
                    )
                    self.reply(200 if updated else 404, {"code": 0 if updated else 404, "data": {}})
                    return
                with fixture.lock:
                    fixture.unclassified += 1
                self.reply(404, {"code": 404, "message": "fixture endpoint not found"})

            def do_DELETE(self) -> None:
                path = urlsplit(self.path).path
                match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)", path)
                if match:
                    account_id = int(match.group(1))
                    self.count(f"account_delete:{account_id}")
                    deleted = self.mutate_account("DELETE FROM accounts WHERE id=%s", (account_id,))
                    self.reply(200 if deleted else 404, {"code": 0 if deleted else 404, "data": {}})
                    return
                with fixture.lock:
                    fixture.unclassified += 1
                self.reply(404, {"code": 404, "message": "fixture endpoint not found"})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       kwargs={"poll_interval": 0.05}, daemon=True)

    def __enter__(self) -> "MutationUpstream":
        self.thread.start()
        return self

    def set_hold(self, account_ids: set[int]) -> None:
        with self.lock:
            self.hold_accounts = set(account_ids)
            self.started = {account_id: threading.Event() for account_id in account_ids}
            self.release = {account_id: threading.Event() for account_id in account_ids}

    def wait_started(self, account_id: int, timeout: float = 8) -> bool:
        with self.lock:
            event = self.started.setdefault(account_id, threading.Event())
        return event.wait(timeout)

    def wait_any_started(self, account_ids: set[int], timeout: float = 8) -> int | None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self.lock:
                found = [account_id for account_id in account_ids
                         if self.started.setdefault(account_id, threading.Event()).is_set()]
            if found:
                return min(found)
            time.sleep(0.02)
        return None

    def release_all(self) -> None:
        with self.lock:
            self.hold_accounts.clear()
            events = list(self.release.values())
        for event in events:
            event.set()

    def count_for(self, name: str) -> int:
        with self.lock:
            return int(self.calls[name])

    def snapshot(self) -> dict[str, int]:
        with self.lock:
            return {**dict(sorted(self.calls.items())),
                    "unclassified": self.unclassified,
                    "fixture_errors": self.fixture_errors}

    def __exit__(self, *_args) -> None:
        self.release_all()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


class Suite:
    def __init__(self, service: Any, mocks: MutationUpstream, database_url: str,
                 command: list[str] | None, temporary: Path) -> None:
        self.service = service
        self.client = http.HttpClient(service.server_url, timeout=2)
        self.mocks = mocks
        self.database_url = database_url
        self.command = command
        self.temporary = temporary
        self.cases: list[dict[str, object]] = []
        self.restart_service = None

    @property
    def headers(self) -> dict[str, str]:
        return {"x-api-key": http.ADMIN_KEY}

    def api(self, method: str, path: str, body: object | None = None):
        return self.client.request(method, PREFIX + path, body=body, headers=self.headers)

    def case(self, name: str, run: Callable[[], dict[str, object]]) -> None:
        before = self.mocks.snapshot()
        try:
            detail = run()
        except MutationFailure as exc:
            self.cases.append({"name": name, "status": "failed",
                               "expected": safe_result(exc.expected),
                               "actual": safe_result(exc.actual)})
        except BaseException as exc:
            self.cases.append({"name": name, "status": "failed",
                               "error": http.safe_failure(exc)})
        else:
            self.cases.append({"name": name, "status": "passed", "detail": safe_result(detail)})
        after = self.mocks.snapshot()
        self.cases[-1]["upstream_delta"] = {
            key: after.get(key, 0) - before.get(key, 0)
            for key in sorted(set(before) | set(after))
            if after.get(key, 0) != before.get(key, 0)
        }

    def db_account(self, account_id: int) -> dict[str, object] | None:
        import psycopg

        with psycopg.connect(self.database_url) as connection:
            row = connection.execute(
                "SELECT id,priority,schedulable,deleted_at FROM accounts WHERE id=%s",
                (account_id,),
            ).fetchone()
        if row is None:
            return None
        return {"id": int(row[0]), "priority": int(row[1]),
                "schedulable": bool(row[2]), "deleted": row[3] is not None}

    def account(self, account_id: int) -> dict[str, Any]:
        response = self.api("GET", "/snapshot")
        check(response.status == 200, {"snapshot_status": 200}, {"snapshot_status": response.status})
        accounts = response.body.get("accounts") if isinstance(response.body, dict) else None
        row = next((item for item in accounts or [] if item.get("id") == account_id), None)
        check(isinstance(row, dict), {"account_id": account_id}, {"account_found": row is not None})
        check(isinstance(row.get("version"), str) and len(row["version"]) == 64,
              {"base_version_length": 64}, {"base_version_length": len(str(row.get("version", "")))})
        check(isinstance(row.get("operation_versions"), dict),
              {"field_versions": True}, {"field_versions": False})
        return row

    def config_roundtrip(self) -> dict[str, object]:
        initial = self.api("GET", "/config")
        check(initial.status == 200, {"status": 200}, {"status": initial.status})
        current = initial.body["oauth"]
        old_time = current["oauth_daily_test_time"]
        next_time = "04:17" if old_time != "04:17" else "04:18"
        stale_time = "04:19" if next_time != "04:19" else "04:20"
        old_revision = current["revision"]
        before_writes = self.mocks.snapshot()
        saved = self.api("PUT", "/config/oauth", {
            "expected_revision": old_revision,
            "changes": {"oauth_daily_test_time": next_time},
        })
        check(saved.status == 200, {"save_status": 200}, {"save_status": saved.status})
        after_save = self.api("GET", "/config")
        check(after_save.status == 200, {"read_after_save_status": 200},
              {"read_after_save_status": after_save.status})
        saved_config = after_save.body["oauth"]
        check(saved_config["oauth_daily_test_time"] == next_time,
              {"saved_time": next_time}, {"saved_time": saved_config.get("oauth_daily_test_time")})
        check(saved_config["revision"] != old_revision,
              {"revision_changed": True}, {"revision_changed": False})
        conflict = self.api("PUT", "/config/oauth", {
            "expected_revision": old_revision,
            "changes": {"oauth_daily_test_time": stale_time},
        })
        check(conflict.status == 409, {"stale_revision_status": 409},
              {"stale_revision_status": conflict.status})
        final = self.api("GET", "/config")
        check(final.status == 200, {"final_read_status": 200}, {"final_read_status": final.status})
        final_config = final.body["oauth"]
        check(final_config["oauth_daily_test_time"] == next_time
              and final_config["revision"] == saved_config["revision"],
              {"time": next_time, "revision_unchanged_after_conflict": True},
              {"time": final_config.get("oauth_daily_test_time"),
               "revision_unchanged_after_conflict": final_config.get("revision") == saved_config["revision"]})
        after_writes = self.mocks.snapshot()
        remote_writes = ("account_update:4001", "account_delete:4004", "schedulable:4002",
                         "sse:4001", "sse:4002", "sse:4003")
        deltas = {key: after_writes.get(key, 0) - before_writes.get(key, 0)
                  for key in remote_writes}
        check(all(value == 0 for value in deltas.values()),
              {"sub2api_mutation_deltas": {key: 0 for key in remote_writes}},
              {"sub2api_mutation_deltas": deltas})
        return {"save_status": saved.status, "stale_status": conflict.status,
                "old_time": old_time, "saved_time": next_time,
                "revision_changed": saved_config["revision"] != old_revision,
                "remote_mutation_calls": deltas}

    def direct_priority(self) -> dict[str, object]:
        row = self.account(4001)
        before_calls = self.mocks.count_for("account_update:4001")
        desired = 11
        response = self.api("POST", "/accounts/4001/priority", {
            "expected_version": row["version"], "priority": desired,
        })
        state = self.db_account(4001)
        calls = self.mocks.count_for("account_update:4001") - before_calls
        check(response.status == 200 and state is not None and state["priority"] == desired and calls == 1,
              {"http_status": 200, "priority": desired, "upstream_put_delta": 1},
              {"http_status": response.status,
               "priority": state.get("priority") if state else None,
               "upstream_put_delta": calls})
        return {"http_status": response.status, "priority": state["priority"],
                "upstream_put_delta": calls}

    def queued_priority(self) -> dict[str, object]:
        row = self.account(4001)
        versions = row["operation_versions"]
        expected = versions.get("priority")
        check(isinstance(expected, str) and len(expected) == 64,
              {"priority_version_length": 64}, {"priority_version_length": len(str(expected or ""))})
        request = {
            "action": "priority", "request_id": "mutation-priority-request-040",
            "client_id": "mutation-priority-client-040", "expected_version": expected,
            "payload": {"priority": 13},
        }
        before_calls = self.mocks.count_for("account_update:4001")
        accepted = self.api("POST", "/accounts/4001/operations", request)
        check(accepted.status == 202 and isinstance(accepted.body, dict) and accepted.body.get("id"),
              {"submit_status": 202, "job_id": True},
              {"submit_status": accepted.status,
               "job_id": isinstance(accepted.body, dict) and bool(accepted.body.get("id"))})
        job_id = accepted.body["id"]
        repeated = self.api("POST", "/accounts/4001/operations", request)
        check(repeated.status == 202 and repeated.body.get("id") == job_id,
              {"repeat_status": 202, "same_job_id": True},
              {"repeat_status": repeated.status,
               "same_job_id": isinstance(repeated.body, dict) and repeated.body.get("id") == job_id})
        terminal = self.wait_job(job_id, {"completed", "failed", "needs_confirmation"})
        state = self.db_account(4001)
        calls = self.mocks.count_for("account_update:4001") - before_calls
        check(terminal.get("status") == "completed" and state is not None
              and state["priority"] == 13 and calls == 1,
              {"status": "completed", "priority": 13, "upstream_put_delta": 1},
              {"status": terminal.get("status"),
               "priority": state.get("priority") if state else None,
               "upstream_put_delta": calls})
        return {"job_status": terminal.get("status"), "job_id_stable": True,
                "priority": state["priority"], "upstream_put_delta": calls}

    def direct_schedulable(self) -> dict[str, object]:
        row = self.account(4002)
        stale_version = row["version"]
        before_calls = self.mocks.count_for("schedulable:4002")
        saved = self.api("POST", "/accounts/4002/schedulable", {
            "expected_version": stale_version, "schedulable": False,
        })
        state = self.db_account(4002)
        first_delta = self.mocks.count_for("schedulable:4002") - before_calls
        check(saved.status == 200 and isinstance(saved.body, dict) and saved.body.get("verified") is True
              and state is not None and state["schedulable"] is False and first_delta == 1,
              {"http_status": 200, "schedulable": False, "verified": True, "upstream_post_delta": 1},
              {"http_status": saved.status,
               "schedulable": state.get("schedulable") if state else None,
               "verified": isinstance(saved.body, dict) and saved.body.get("verified") is True,
               "upstream_post_delta": first_delta})
        stale = self.api("POST", "/accounts/4002/schedulable", {
            "expected_version": stale_version, "schedulable": True,
        })
        state_after_stale = self.db_account(4002)
        total_delta = self.mocks.count_for("schedulable:4002") - before_calls
        check(stale.status == 409 and total_delta == 1 and state_after_stale is not None
              and state_after_stale["schedulable"] is False,
              {"stale_status": 409, "schedulable": False, "upstream_post_delta": 1},
              {"stale_status": stale.status,
               "schedulable": state_after_stale.get("schedulable") if state_after_stale else None,
               "upstream_post_delta": total_delta})
        return {"save_status": saved.status, "stale_status": stale.status,
                "schedulable": state_after_stale["schedulable"],
                "upstream_post_delta": total_delta}

    def delete_account(self) -> dict[str, object]:
        row = self.account(4004)
        before_calls = self.mocks.count_for("account_delete:4004")
        response = self.api("DELETE", "/accounts/4004", {"expected_version": row["version"]})
        state = self.db_account(4004)
        first_delta = self.mocks.count_for("account_delete:4004") - before_calls
        check(response.status == 200 and isinstance(response.body, dict)
              and response.body.get("deleted") is True and state is None and first_delta == 1,
              {"http_status": 200, "deleted": True, "account_absent": True, "upstream_delete_delta": 1},
              {"http_status": response.status,
               "deleted": isinstance(response.body, dict) and response.body.get("deleted") is True,
               "account_absent": state is None, "upstream_delete_delta": first_delta})
        stale = self.api("DELETE", "/accounts/4004", {"expected_version": row["version"]})
        total_delta = self.mocks.count_for("account_delete:4004") - before_calls
        check(stale.status == 404 and self.db_account(4004) is None and total_delta == 1,
              {"stale_status": 404, "account_absent": True, "upstream_delete_delta": 1},
              {"stale_status": stale.status, "account_absent": self.db_account(4004) is None,
               "upstream_delete_delta": total_delta})
        return {"delete_status": response.status, "stale_status": stale.status,
                "account_absent": True, "upstream_delete_delta": total_delta}

    def wait_job(self, job_id: str, terminal: set[str], timeout: float = 10) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            response = self.api("GET", f"/account-operations/{job_id}")
            check(response.status == 200 and isinstance(response.body, dict),
                  {"job_read_status": 200}, {"job_read_status": response.status})
            latest = response.body
            if latest.get("status") in terminal:
                return latest
            time.sleep(0.05)
        return latest

    def wait_batch(self, batch_id: str, predicate: Callable[[dict[str, Any]], bool],
                   timeout: float = 10) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            response = self.api("GET", f"/account-operations?batch_id={batch_id}")
            check(response.status == 200 and isinstance(response.body, dict),
                  {"batch_read_status": 200}, {"batch_read_status": response.status})
            latest = response.body
            if predicate(latest):
                return latest
            time.sleep(0.05)
        return latest

    @staticmethod
    def statuses(batch: dict[str, Any]) -> dict[int, str]:
        return {int(item["account_id"]): str(item["status"])
                for item in batch.get("items", []) if isinstance(item, dict)}

    def batch_idempotent_cancel(self) -> dict[str, object]:
        account_rows = [self.account(account_id) for account_id in (4001, 4002)]
        versions = {row["id"]: row["operation_versions"]["test"] for row in account_rows}
        self.mocks.set_hold({4001, 4002})
        request = {
            "action": "test", "payload": {"confirmed": True},
            "accounts": [{"account_id": account_id, "expected_version": versions[account_id]}
                         for account_id in (4001, 4002)],
            "request_id": "mutation-test-batch-request-040",
            "client_id": "mutation-test-batch-client-040",
        }
        try:
            before = {account_id: self.mocks.count_for(f"sse:{account_id}")
                      for account_id in (4001, 4002)}
            submitted = self.api("POST", "/account-operations/batch", request)
            check(submitted.status == 202 and isinstance(submitted.body, dict)
                  and isinstance(submitted.body.get("batch_id"), str),
                  {"submit_status": 202, "batch_id": True},
                  {"submit_status": submitted.status,
                   "batch_id": isinstance(submitted.body, dict)
                   and isinstance(submitted.body.get("batch_id"), str)})
            batch_id = submitted.body["batch_id"]
            active_id = self.mocks.wait_any_started({4001, 4002}, 8)
            check(active_id is not None, {"held_sse_started": True}, {"held_sse_started": False})
            queued_id = 4002 if active_id == 4001 else 4001
            active = self.wait_batch(batch_id, lambda value: self.statuses(value).get(active_id) == "running")
            active_statuses = self.statuses(active)
            check(active_statuses.get(queued_id) == "queued",
                  {"active_job": "running", "other_job": "queued"},
                  {"statuses": active_statuses})

            duplicate = self.api("POST", "/account-operations/batch", request)
            duplicate_ids = sorted(item.get("id") for item in duplicate.body.get("items", [])) \
                if isinstance(duplicate.body, dict) else []
            original_ids = sorted(item.get("id") for item in submitted.body.get("items", []))
            check(duplicate.status == 202 and duplicate.body.get("batch_id") == batch_id
                  and duplicate_ids == original_ids,
                  {"repeat_status": 202, "same_batch_id": True, "same_job_ids": True},
                  {"repeat_status": duplicate.status,
                   "same_batch_id": isinstance(duplicate.body, dict)
                   and duplicate.body.get("batch_id") == batch_id,
                   "same_job_ids": duplicate_ids == original_ids})
            after_duplicate = {account_id: self.mocks.count_for(f"sse:{account_id}")
                               for account_id in (4001, 4002)}
            check(after_duplicate == {account_id: before[account_id] + (1 if account_id == active_id else 0)
                                      for account_id in (4001, 4002)},
                  {"one_started_sse": True, "duplicate_requests": 0},
                  {"sse_calls": {str(key): after_duplicate[key] - before[key]
                                 for key in before}})

            cancelled = self.api("POST", f"/account-operations/batches/{batch_id}/cancel", {})
            check(cancelled.status == 200 and isinstance(cancelled.body, dict),
                  {"cancel_status": 200}, {"cancel_status": cancelled.status})
            cancelled_statuses = self.statuses(cancelled.body)
            check(cancelled_statuses.get(active_id) == "running"
                  and cancelled_statuses.get(queued_id) == "cancelled",
                  {"active_job": "running", "queued_job": "cancelled"},
                  {"statuses": cancelled_statuses})
            self.mocks.release_all()
            final = self.wait_batch(batch_id, lambda value:
                                    self.statuses(value).get(active_id) == "completed"
                                    and self.statuses(value).get(queued_id) == "cancelled")
            final_statuses = self.statuses(final)
            final_counts = {account_id: self.mocks.count_for(f"sse:{account_id}") - before[account_id]
                            for account_id in before}
            check(final_statuses.get(active_id) == "completed"
                  and final_statuses.get(queued_id) == "cancelled"
                  and final_counts == {active_id: 1, queued_id: 0},
                  {"statuses": {active_id: "completed", queued_id: "cancelled"},
                   "sse_deltas": {active_id: 1, queued_id: 0}},
                  {"statuses": final_statuses, "sse_deltas": final_counts})
            return {"batch_id_stable": True, "same_job_ids": True,
                    "active_account": active_id, "queued_account": queued_id,
                    "final_statuses": final_statuses, "sse_deltas": final_counts}
        finally:
            self.mocks.release_all()

    def kill_service(self, service: Any) -> int | None:
        process = service.process
        if process is None:
            return service.exit_code
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
        service.exit_code = process.returncode
        service.process = None
        return service.exit_code

    def unknown_restart(self) -> dict[str, object]:
        row = self.account(4003)
        request = {
            "action": "test", "payload": {"confirmed": True},
            "accounts": [{"account_id": 4003,
                          "expected_version": row["operation_versions"]["test"]}],
            "request_id": "mutation-restart-batch-request-040",
            "client_id": "mutation-restart-batch-client-040",
        }
        self.mocks.set_hold({4003})
        before = self.mocks.count_for("sse:4003")
        try:
            submitted = self.api("POST", "/account-operations/batch", request)
            check(submitted.status == 202 and isinstance(submitted.body, dict)
                  and isinstance(submitted.body.get("batch_id"), str),
                  {"submit_status": 202, "batch_id": True},
                  {"submit_status": submitted.status,
                   "batch_id": isinstance(submitted.body, dict)
                   and isinstance(submitted.body.get("batch_id"), str)})
            batch_id = submitted.body["batch_id"]
            check(self.mocks.wait_started(4003, 8),
                  {"held_sse_started": True}, {"held_sse_started": False})
            running = self.wait_batch(batch_id,
                                      lambda value: self.statuses(value).get(4003) == "running")
            check(self.statuses(running).get(4003) == "running",
                  {"before_restart_status": "running"},
                  {"before_restart_status": self.statuses(running).get(4003)})

            killed_exit = self.kill_service(self.service)
            restart_root = self.temporary / "service-restart"
            restart_root.mkdir(mode=0o700)
            restarted = http.ServiceProcess(
                restart_root, self.service.state_dir, http.free_port(), self.mocks,
                self.database_url, command=self.command,
                reference_app=self.service.reference_app,
                target="command" if self.command else "python-reference",
            )
            self.restart_service = restarted
            restarted.start()
            self.client = http.HttpClient(restarted.server_url, timeout=2)
            final = self.wait_batch(batch_id,
                                    lambda value: self.statuses(value).get(4003)
                                    in {"needs_confirmation", "completed", "failed"})
            final_status = self.statuses(final).get(4003)
            count = self.mocks.count_for("sse:4003") - before
            check(final_status == "needs_confirmation" and count == 1,
                  {"after_restart_status": "needs_confirmation", "upstream_sse_calls": 1},
                  {"after_restart_status": final_status, "upstream_sse_calls": count})
            time.sleep(0.2)
            stable = self.wait_batch(batch_id, lambda value: True)
            stable_status = self.statuses(stable).get(4003)
            stable_count = self.mocks.count_for("sse:4003") - before
            check(stable_status == "needs_confirmation" and stable_count == 1,
                  {"stable_status": "needs_confirmation", "upstream_sse_calls": 1},
                  {"stable_status": stable_status, "upstream_sse_calls": stable_count})
            return {"pre_restart_status": "running", "after_restart_status": stable_status,
                    "upstream_sse_calls": stable_count,
                    "old_process_exit_code": killed_exit, "restarted_service": True}
        finally:
            self.mocks.release_all()

    def run(self) -> None:
        self.case("config_save_and_stale_revision_conflict", self.config_roundtrip)
        self.case("manual_priority_write_and_database_readback", self.direct_priority)
        self.case("queued_priority_idempotency_and_readback", self.queued_priority)
        self.case("manual_schedulable_write_and_stale_version", self.direct_schedulable)
        self.case("manual_delete_and_stale_version", self.delete_account)
        self.case("batch_omitted_default_payload_preserves_idempotency", self.batch_default_payload)
        self.case("template_account_integer_normalization_preserves_idempotency", self.template_account_integer)
        self.case("test_batch_idempotency_cancellation_and_sse_counts", self.batch_idempotent_cancel)
        self.case("unknown_test_request_after_restart_is_not_replayed", self.unknown_restart)
        self.case("all_upstream_paths_are_classified", self.classified_upstream)

    def batch_default_payload(self) -> dict[str, object]:
        # A nonexistent synthetic account keeps this validation check free of
        # upstream writes while exercising the complete HTTP acceptance path.
        request = {"action": "groups", "request_id": "fixture-default-payload-040",
                   "client_id": "fixture-default-client-040",
                   "accounts": [{"account_id": 989998, "expected_version": "a" * 64,
                                 "payload": {"scope_group_ids": [1], "group_ids": []}}]}
        accepted = self.api("POST", "/account-operations/batch", request)
        check(accepted.status == 202 and isinstance(accepted.body, dict)
              and bool(accepted.body.get("batch_id")),
              {"status": 202, "accepted": True},
              {"status": accepted.status, "accepted": isinstance(accepted.body, dict)
               and bool(accepted.body.get("batch_id"))})
        request["payload"] = {}
        repeated = self.api("POST", "/account-operations/batch", request)
        check(repeated.status == 202 and isinstance(repeated.body, dict)
              and repeated.body.get("batch_id") == accepted.body["batch_id"],
              {"status": 202, "same_batch": True},
              {"status": repeated.status, "same_batch": isinstance(repeated.body, dict)
               and repeated.body.get("batch_id") == accepted.body["batch_id"]})
        check(len(repeated.body["items"]) == 1 and repeated.body["items"][0]["status"] == "needs_confirmation",
              {"items": 1, "status": "needs_confirmation"},
              {"items": len(repeated.body["items"]), "status": repeated.body["items"][0]["status"]})
        return {"status": 202, "same_batch": True, "missing_account_status": "needs_confirmation"}

    def classified_upstream(self) -> dict[str, object]:
        calls = self.mocks.snapshot()
        actual = {key: calls.get(key, 0) for key in ("unclassified", "fixture_errors")}
        check(actual == {"unclassified": 0, "fixture_errors": 0},
              {"unclassified": 0, "fixture_errors": 0}, actual)
        return actual

    def template_account_integer(self) -> dict[str, object]:
        templates = self.api("GET", "/account-templates")
        check(templates.status == 200, {"status": 200}, {"status": templates.status})
        if not templates.body["configured"]:
            payload = {"expected_version": templates.body["version"], **templates.body["templates"]}
            templates = self.api("PUT", "/account-templates", payload)
            check(templates.status == 200, {"status": 200}, {"status": templates.status})
        request = {"template_id": "full", "template_version": templates.body["template_versions"]["full"],
                   "request_id": "0123456789abcdef0123456789abcdef",
                   "client_id": "fedcba9876543210fedcba9876543210",
                   "accounts": [{"account_id": "989997", "expected_version": "a" * 64}]}
        accepted = self.api("POST", "/account-templates/apply", request)
        check(accepted.status == 202, {"status": 202}, {"status": accepted.status})
        batch_id = accepted.body["batch_id"]
        for value in (989997, 989997.0, "989997.000", "989_997"):
            request["accounts"][0]["account_id"] = value
            repeated = self.api("POST", "/account-templates/apply", request)
            check(repeated.status == 202 and repeated.body.get("batch_id") == batch_id,
                  {"status": 202, "same_batch": True},
                  {"status": repeated.status, "same_batch": repeated.body.get("batch_id") == batch_id})
            check(repeated.body["items"][0]["account_id"] == 989997,
                  {"normalized_id": 989997}, {"normalized_id": repeated.body["items"][0]["account_id"]})
        return {"normalized_variants": 5, "same_batch": True, "upstream_writes": 0}


def seed_accounts(database_url: str) -> None:
    import psycopg

    credentials = json.dumps({"access_token": "fixture-account-token-mutations-040",
                              "model_mapping": {}})
    with psycopg.connect(database_url, autocommit=True) as connection:
        connection.execute(
            "UPDATE accounts SET priority=3,schedulable=false,status='active',deleted_at=NULL,updated_at=now() WHERE id=4001"
        )
        for account_id, name, schedulable in (
            (4002, "fixture-schedule-account-040", True),
            (4003, "fixture-restart-account-040", True),
            (4004, "fixture-delete-account-040", True),
        ):
            connection.execute(
                "INSERT INTO accounts (id,name,platform,type,status,schedulable,credentials,extra,priority,concurrency,load_factor) "
                "VALUES (%s,%s,'openai','oauth','active',%s,%s::jsonb,'{}'::jsonb,3,2,1) "
                "ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name,platform=EXCLUDED.platform,type=EXCLUDED.type,"
                "status=EXCLUDED.status,schedulable=EXCLUDED.schedulable,credentials=EXCLUDED.credentials,"
                "extra=EXCLUDED.extra,priority=EXCLUDED.priority,concurrency=EXCLUDED.concurrency,"
                "load_factor=EXCLUDED.load_factor,deleted_at=NULL,updated_at=now()",
                (account_id, name, schedulable, credentials),
            )


def file_hashes(command: list[str] | None) -> dict[str, object]:
    target: dict[str, object] = {
        "runner_sha256": sha256(Path(__file__).resolve()),
        "reference_sha": REFERENCE_SHA,
    }
    if command:
        binary = Path(command[0]).expanduser().resolve()
        target["binary_sha256"] = sha256(binary) if binary.is_file() else None
    return target


def run_owned(command: list[str] | None, postgres_bin_dir: str | None) -> dict[str, object]:
    result: dict[str, object] = {
        "status": "failed", "target": "command" if command else "python-reference",
        "source": file_hashes(command), "cases": [], "exit_code": 1,
    }
    if IMPORT_ERROR:
        result["error"] = "existing temporary PostgreSQL runner unavailable: " + IMPORT_ERROR
        return result
    if command:
        binary = Path(command[0]).expanduser().resolve()
        if not binary.is_file() or not os.access(binary, os.X_OK):
            result["error"] = "specified command binary is unavailable"
            return result
        command = [str(binary), *command[1:]]

    postgres = None
    services: list[Any] = []
    temporary_owner = None
    temporary_path: Path | None = None
    cleanup: dict[str, object] = {"service_processes": "not_started",
                                  "postgres_process": "not_started",
                                  "temporary_files": "not_created"}
    try:
        temporary_owner = tempfile.TemporaryDirectory(prefix="server-mutation-contract-040-")
        temporary_path = Path(temporary_owner.name)
        state_dir = temporary_path / "state"
        state_dir.mkdir(mode=0o700)
        reference_app = None
        if command is None:
            reference_app = http.archive_reference_app(temporary_path / "reference", REFERENCE_SHA)
            http.seed_fingerprint_cache(reference_app, state_dir)
        postgres = http.TemporaryPostgres(temporary_path, postgres_bin_dir)
        postgres.start()
        seed_accounts(postgres.url)
        result["database"] = {"kind": "runner-owned temporary PostgreSQL",
                              "loopback_only": True, "pid": postgres.postgres_pid,
                              "accounts": list(ACCOUNT_IDS), "probe_accounts_used": False}
        with MutationUpstream(postgres.url) as mocks:
            service_root = temporary_path / "service"
            service_root.mkdir(mode=0o700)
            service = http.ServiceProcess(
                service_root, state_dir, http.free_port(), mocks,
                postgres.url, command=command, reference_app=reference_app,
                target="command" if command else "python-reference",
            )
            services.append(service)
            service.start()
            suite = Suite(service, mocks, postgres.url, command, temporary_path)
            result["service"] = {"owned_by_runner": True, "host": "127.0.0.1",
                                  "pid": service.process.pid if service.process else None}
            try:
                suite.run()
            finally:
                if suite.restart_service is not None:
                    services.append(suite.restart_service)
                mocks.release_all()
                for item in reversed(services):
                    try:
                        item.close()
                    except Exception:
                        pass
            result["cases"] = suite.cases
            result["external_calls"] = mocks.snapshot()
            failed = [case for case in suite.cases if case.get("status") == "failed"]
            result["primary_suite_status"] = "failed" if failed else "passed"
            result["service_exit_codes"] = [item.exit_code for item in services]
            cleanup["service_processes"] = "stopped"
        cleanup["upstream_mock"] = "stopped"
        postgres.close()
        cleanup["postgres_process"] = "stopped"
        temporary_owner.cleanup()
        cleanup["temporary_files"] = "removed" if temporary_path and not temporary_path.exists() else "not_removed"
        if cleanup["temporary_files"] != "removed":
            raise RuntimeError("runner-owned temporary directory remains")
        result["temporary_directory_removed"] = True
        result["cleanup"] = cleanup
        failed = any(case.get("status") == "failed" for case in result.get("cases", []))
        result["status"] = "failed" if failed else "passed"
        result["exit_code"] = 1 if failed else 0
    except BaseException as exc:
        for item in reversed(services):
            try:
                item.close()
            except Exception:
                pass
        if postgres is not None:
            try:
                postgres.close()
                cleanup["postgres_process"] = "stopped"
            except Exception:
                cleanup["postgres_process"] = "stop_failed"
        if temporary_owner is not None and cleanup["postgres_process"] != "stop_failed":
            try:
                temporary_owner.cleanup()
                cleanup["temporary_files"] = "removed"
            except Exception:
                cleanup["temporary_files"] = "remove_failed"
        result["status"] = "failed"
        result["error"] = type(exc).__name__
        result["exit_code"] = 1
        result["cleanup"] = cleanup
        if temporary_path is not None:
            result["temporary_directory_removed"] = not temporary_path.exists()
    if command and "binary_sha256" in result["source"]:
        binary = Path(command[0]).expanduser().resolve()
        result["source"]["binary_sha256_after"] = sha256(binary) if binary.is_file() else None
        result["source"]["binary_unchanged_during_run"] = (
            result["source"].get("binary_sha256") == result["source"].get("binary_sha256_after")
        )
    return result


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group()
    target.add_argument("--command-json", type=http.parse_command if http else None,
                        help="JSON argv for the existing target binary; supports {host} and {port}")
    parser.add_argument("--postgres-bin-dir", help="directory containing initdb and pg_ctl")
    return parser.parse_args()


def main() -> int:
    try:
        args = parse_args()
        result = run_owned(args.command_json, args.postgres_bin_dir)
    except SystemExit as exc:
        return int(exc.code or 0)
    except BaseException as exc:
        result = {"status": "failed", "error": type(exc).__name__, "exit_code": 1,
                  "source": {"runner_sha256": sha256(Path(__file__).resolve()),
                             "reference_sha": REFERENCE_SHA}}
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return int(result["exit_code"])


if __name__ == "__main__":
    raise SystemExit(main())
