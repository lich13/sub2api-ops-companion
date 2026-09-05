from __future__ import annotations

import json
import os
import stat
import tempfile
import threading
import time
import unittest
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from urllib.parse import unquote

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from starlette.datastructures import FormData

from app import account_ops, main as main_module
from app.key_fallback import (
    AVAILABLE,
    DISPATCH_BUDGET_SECONDS,
    SCHEDULABLE_REQUEST_TIMEOUT_SECONDS,
    UNAVAILABLE,
    UNKNOWN,
    KeyFallbackConfigError,
    KeyFallbackController,
    classify_oauth_account,
    desired_key_schedulable,
    execute_sub2api_set_schedulable,
    latest_completed_oauth_result,
)
from app.oauth_monitor import OAuthMonitor
from app.settings import load_settings
from app.telegram_bot import TelegramOpsBot
from app.usage_query import execute_oauth_usage_query, oauth_quota_summary_from_result

NOW = datetime(2026, 9, 5, 8, 0, tzinfo=timezone.utc)
NIGHT = datetime(2026, 9, 4, 17, 30, tzinfo=timezone.utc)
FRESHNESS = 3600


def oauth_row(account_id: int = 1, **overrides: object) -> dict[str, object]:
    row: dict[str, object] = {
        "id": account_id,
        "name": f"oauth-{account_id}",
        "platform": "openai",
        "type": "oauth",
        "credentials": {"plan_type": "plus"},
        "extra": {},
        "status": "active",
        "schedulable": True,
        "temp_unschedulable_until": None,
        "rate_limited_at": None,
        "rate_limit_reset_at": None,
        "overload_until": None,
        "error_message": "stale error text",
        "expires_at": None,
        "auto_pause_on_expired": False,
        "deleted_at": None,
    }
    row.update(overrides)
    return row


def key_row(account_id: int, *, schedulable: bool = False, **overrides: object) -> dict[str, object]:
    row: dict[str, object] = {
        "id": account_id,
        "name": f"key-{account_id}",
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": schedulable,
        "credentials": {"api_key": "secret-key-material"},
        "deleted_at": None,
    }
    row.update(overrides)
    return row


def success_result(
    *,
    plan: str = "plus",
    five: float = 20,
    seven: float = 30,
    queried_at: datetime = NOW,
) -> dict[str, object]:
    windows = []
    if plan != "free":
        windows.append(
            {
                "key": "codex_5h",
                "label": "5h",
                "used_percent": five,
                "remaining_percent": max(0, 100 - five),
            }
        )
    windows.append(
        {
            "key": "codex_7d",
            "label": "7d",
            "used_percent": seven,
            "remaining_percent": max(0, 100 - seven),
        }
    )
    return {
        "success": True,
        "queried_at": queried_at.isoformat(),
        "oauth_quota": {"plan_type": plan, "ui_windows": windows},
    }


def failure_result(error_code: str, queried_at: datetime = NOW) -> dict[str, object]:
    return {
        "success": False,
        "queried_at": queried_at.isoformat(),
        "error_code": error_code,
        "error": "query failed",
    }


class SnapshotEvaluationGuard:
    def __init__(self, monitor: SnapshotMonitor) -> None:
        self._monitor = monitor
        self._held = False

    def __enter__(self) -> SnapshotEvaluationGuard | None:
        self._monitor.calls += 1
        if self._monitor.busy:
            return None
        if not self._monitor._guard_lock.acquire(blocking=False):
            return None
        self._held = True
        return self

    def __exit__(self, *_exc: object) -> None:
        if self._held:
            self._monitor._guard_lock.release()
            self._held = False

    def committed_snapshot(self) -> dict[str, object] | None:
        return self._monitor.snapshot


class SnapshotMonitor:
    def __init__(self, snapshot: dict[str, object] | None, *, busy: bool = False) -> None:
        self.snapshot = snapshot or {"oauth_results": {}, "scheduler": {}, "pending_events": {}}
        self.busy = busy
        self.calls = 0
        self.commits = 0
        self._guard_lock = threading.Lock()

    def evaluation_guard(self) -> SnapshotEvaluationGuard:
        return SnapshotEvaluationGuard(self)

    def committed_snapshot(self) -> dict[str, object] | None:
        if not self._guard_lock.acquire(blocking=False):
            return None
        try:
            if self.busy:
                return None
            return self.snapshot
        finally:
            self._guard_lock.release()

    def store(self) -> SimpleNamespace:
        return SimpleNamespace(commit=self._commit)

    def _commit(self, **_kwargs: object) -> None:
        self.commits += 1


def fallback_settings(root: Path) -> SimpleNamespace:
    return SimpleNamespace(
        key_fallback_config_path=str(root / "key-fallback-config.json"),
        audit_path=str(root / "audit.jsonl"),
        telegram_oauth_regular_refresh_interval_seconds=3600,
        usage_query_state_path=str(root / "usage-query-state.json"),
    )


def make_controller(
    root: Path,
    *,
    oauth_rows: list[dict[str, object]] | None = None,
    key_rows: list[dict[str, object]] | None = None,
    snapshot: dict[str, object] | None = None,
    busy: bool = False,
    runner: object | None = None,
    monitor: SnapshotMonitor | None = None,
    account_reader: object | None = None,
    monotonic: object | None = None,
) -> tuple[KeyFallbackController, SnapshotMonitor, list[tuple[int, bool]]]:
    calls: list[tuple[int, bool]] = []
    monitor = monitor or SnapshotMonitor(snapshot, busy=busy)
    source_keys = key_rows

    def default_runner(account_id: int, schedulable: bool, **_kwargs: object) -> dict[str, object]:
        calls.append((int(account_id), bool(schedulable)))
        return {"success": True}

    def default_reader(_db: object, account_id: int) -> dict[str, object] | None:
        for row in list(source_keys or []):
            if not isinstance(row, dict):
                continue
            try:
                current_id = int(row.get("id") or 0)
            except (TypeError, ValueError):
                continue
            if current_id != int(account_id) or isinstance(row.get("id"), bool):
                continue
            if row.get("deleted_at") not in (None, ""):
                return None
            return dict(row)
        return None

    controller = KeyFallbackController(
        fallback_settings(root),
        db=object(),
        oauth_monitor=monitor,
        base_url_provider=lambda: "http://127.0.0.1:9",
        admin_token_provider=lambda: "admin-test-token",
        oauth_inventory=lambda _db: list(oauth_rows or []),
        key_inventory=lambda _db: list(key_rows or []),
        account_reader=account_reader or default_reader,  # type: ignore[arg-type]
        schedulable_runner=runner or default_runner,  # type: ignore[arg-type]
        monotonic=monotonic or time.monotonic,  # type: ignore[arg-type]
    )
    return controller, monitor, calls


def offline_monitor_kwargs(row: dict[str, object] | None = None) -> dict[str, object]:
    def account_reader(_db: object, account_id: int) -> dict[str, object] | None:
        if row is None or int(row.get("id") or 0) != int(account_id):
            return None
        return dict(row)

    return {
        "test_runner": lambda *_args, **_kwargs: {"success": True, "duration_ms": 1},
        "recovery_runner": lambda *_args, **_kwargs: {"success": True, "method": "recover-state"},
        "account_reader": account_reader,
        "inventory_loader": lambda _db: [dict(row)] if row else [],
    }


def monitor_settings(root: Path, *, recovery_enabled: bool = False) -> SimpleNamespace:
    return SimpleNamespace(
        usage_query_state_path=str(root / "usage-query-state.json"),
        audit_path=str(root / "audit.jsonl"),
        key_fallback_config_path=str(root / "key-fallback-config.json"),
        telegram_oauth_usage_refresh_enabled=True,
        telegram_oauth_recovery_monitor_enabled=recovery_enabled,
        telegram_oauth_night_recovery_cooldown_enabled=True,
        telegram_oauth_usage_refresh_concurrency=1,
        telegram_oauth_recovery_test_concurrency=1,
        telegram_oauth_early_probe_batch_size=8,
        telegram_oauth_regular_refresh_interval_seconds=3600,
        telegram_oauth_7d_probe_interval_seconds=3600,
        telegram_oauth_recovery_test_model_id="gpt-5.6-luna",
    )


def write_monitor_state(
    root: Path,
    *,
    oauth_results: dict[str, object] | None = None,
    scheduler: dict[str, object] | None = None,
) -> Path:
    path = root / "usage-query-state.json"
    path.write_text(
        json.dumps(
            {
                "version": 3,
                "settings": {"sub2api_admin_token": "admin-test-token"},
                "oauth_results": oauth_results or {},
                "scheduler": scheduler or {},
                "pending_events": {},
            }
        ),
        encoding="utf-8",
    )
    return path


class AdminCapture:
    def __init__(self, responses: list[tuple[int, object]] | None = None) -> None:
        self.requests: list[dict[str, object]] = []
        self.responses = list(responses) if responses is not None else None
        capture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length)
                payload = json.loads(raw.decode("utf-8")) if raw else {}
                capture.requests.append(
                    {
                        "path": self.path,
                        "content_type": self.headers.get("Content-Type"),
                        "api_key": self.headers.get("x-api-key"),
                        "payload": payload,
                    }
                )
                if capture.responses is None:
                    account_id = int(self.path.rstrip("/").split("/")[-2])
                    status, body = 200, {
                        "code": 0,
                        "data": {
                            "id": account_id,
                            "schedulable": payload.get("schedulable"),
                        },
                    }
                else:
                    status, body = (
                        capture.responses.pop(0)
                        if capture.responses
                        else (200, {"code": 0, "data": {"schedulable": True}})
                    )
                if isinstance(body, bytes):
                    encoded = body
                    content_type = "text/html; charset=utf-8"
                elif isinstance(body, str):
                    encoded = body.encode("utf-8")
                    content_type = "text/html; charset=utf-8"
                else:
                    encoded = json.dumps(body).encode("utf-8")
                    content_type = "application/json"
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def __enter__(self) -> AdminCapture:
        self.thread.start()
        return self

    def __exit__(self, *_args: object) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class OAuthClassificationTests(unittest.TestCase):
    def test_available_requires_gates_fresh_success_and_required_windows(self) -> None:
        self.assertEqual(
            classify_oauth_account(oauth_row(), success_result(), NOW, FRESHNESS),
            AVAILABLE,
        )
        free = oauth_row(credentials={"plan_type": "free"})
        self.assertEqual(
            classify_oauth_account(free, success_result(plan="free", seven=10), NOW, FRESHNESS),
            AVAILABLE,
        )

    def test_blocking_db_state_is_unavailable_and_stale_fields_do_not_block(self) -> None:
        self.assertEqual(
            classify_oauth_account(oauth_row(status="disabled"), success_result(), NOW, FRESHNESS),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(oauth_row(schedulable=False), success_result(), NOW, FRESHNESS),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(
                oauth_row(temp_unschedulable_until=(NOW + timedelta(minutes=5)).isoformat()),
                success_result(),
                NOW,
                FRESHNESS,
            ),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(
                oauth_row(rate_limit_reset_at=(NOW + timedelta(minutes=1)).isoformat()),
                success_result(),
                NOW,
                FRESHNESS,
            ),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(
                oauth_row(overload_until=(NOW + timedelta(minutes=1)).isoformat()),
                success_result(),
                NOW,
                FRESHNESS,
            ),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(
                oauth_row(
                    auto_pause_on_expired=True,
                    expires_at=(NOW - timedelta(minutes=1)).isoformat(),
                ),
                success_result(),
                NOW,
                FRESHNESS,
            ),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(
                oauth_row(
                    rate_limited_at=(NOW - timedelta(hours=1)).isoformat(),
                    rate_limit_reset_at=(NOW - timedelta(minutes=1)).isoformat(),
                    temp_unschedulable_until=(NOW - timedelta(minutes=1)).isoformat(),
                    error_message="old",
                ),
                success_result(),
                NOW,
                FRESHNESS,
            ),
            AVAILABLE,
        )

    def test_quota_exhaustion_auth_errors_and_incomplete_windows(self) -> None:
        self.assertEqual(
            classify_oauth_account(oauth_row(), success_result(five=100, seven=10), NOW, FRESHNESS),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(oauth_row(), failure_result("http_401"), NOW, FRESHNESS),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(oauth_row(), failure_result("http_402"), NOW, FRESHNESS),
            UNAVAILABLE,
        )
        incomplete = success_result(seven=10)
        incomplete["oauth_quota"] = {
            "plan_type": "plus",
            "ui_windows": [{"key": "codex_7d", "used_percent": 10}],
        }
        self.assertEqual(classify_oauth_account(oauth_row(), incomplete, NOW, FRESHNESS), UNKNOWN)

    def test_freshness_and_ordinary_errors_are_unknown(self) -> None:
        stale = success_result(queried_at=NOW - timedelta(seconds=3601))
        self.assertEqual(classify_oauth_account(oauth_row(), stale, NOW, FRESHNESS), UNKNOWN)
        future = success_result(queried_at=NOW + timedelta(seconds=5))
        self.assertEqual(classify_oauth_account(oauth_row(), future, NOW, FRESHNESS), UNKNOWN)
        missing = dict(success_result())
        missing.pop("queried_at")
        self.assertEqual(classify_oauth_account(oauth_row(), missing, NOW, FRESHNESS), UNKNOWN)
        invalid = dict(success_result())
        invalid["queried_at"] = "not-a-time"
        self.assertEqual(classify_oauth_account(oauth_row(), invalid, NOW, FRESHNESS), UNKNOWN)
        self.assertEqual(
            classify_oauth_account(oauth_row(), failure_result("network_error"), NOW, FRESHNESS),
            UNKNOWN,
        )
        self.assertEqual(
            classify_oauth_account(oauth_row(), failure_result("timeout"), NOW, FRESHNESS),
            UNKNOWN,
        )
        self.assertEqual(
            classify_oauth_account(oauth_row(), failure_result("http_500"), NOW, FRESHNESS),
            UNKNOWN,
        )
        stale_auth = failure_result("http_401", queried_at=NOW - timedelta(hours=2))
        self.assertEqual(classify_oauth_account(oauth_row(), stale_auth, NOW, FRESHNESS), UNKNOWN)

    def test_desired_state_matrix(self) -> None:
        self.assertIsNone(desired_key_schedulable([]))
        self.assertFalse(desired_key_schedulable([AVAILABLE, UNAVAILABLE, UNKNOWN]))
        self.assertTrue(desired_key_schedulable([UNAVAILABLE, UNAVAILABLE]))
        self.assertIsNone(desired_key_schedulable([UNAVAILABLE, UNKNOWN]))
        self.assertIsNone(desired_key_schedulable([UNKNOWN, UNKNOWN]))

    def test_latest_completed_attempt_prefers_newer_success_over_leftover_error(self) -> None:
        older = NOW - timedelta(minutes=10)
        newer = NOW
        prior = success_result(queried_at=older)
        failure = latest_completed_oauth_result(
            prior,
            {"last_error_code": "http_401", "last_error_at": newer.isoformat(), "last_success_at": older.isoformat()},
        )
        self.assertFalse((failure or {}).get("success"))
        self.assertEqual((failure or {}).get("error_code"), "http_401")
        recovered = latest_completed_oauth_result(
            success_result(queried_at=newer),
            {
                "last_error_code": "http_401",
                "last_error_at": older.isoformat(),
                "last_success_at": newer.isoformat(),
            },
        )
        self.assertTrue((recovered or {}).get("success"))
        self.assertEqual(
            classify_oauth_account(oauth_row(), failure, NOW, FRESHNESS),
            UNAVAILABLE,
        )
        self.assertEqual(
            classify_oauth_account(oauth_row(), recovered, NOW, FRESHNESS),
            AVAILABLE,
        )
        stale = latest_completed_oauth_result(
            None,
            {
                "last_error_code": "http_401",
                "last_error_at": (NOW - timedelta(hours=2)).isoformat(),
            },
        )
        self.assertEqual(classify_oauth_account(oauth_row(), stale, NOW, FRESHNESS), UNKNOWN)
        future = latest_completed_oauth_result(
            None,
            {"last_error_code": "http_401", "last_error_at": (NOW + timedelta(minutes=1)).isoformat()},
        )
        self.assertEqual(classify_oauth_account(oauth_row(), future, NOW, FRESHNESS), UNKNOWN)
        invalid = latest_completed_oauth_result(
            None,
            {"last_error_code": "http_401", "last_error_at": "not-a-time"},
        )
        self.assertEqual(classify_oauth_account(oauth_row(), invalid, NOW, FRESHNESS), UNKNOWN)

    def test_success_without_fresh_quota_payload_is_unknown_despite_stale_extra(self) -> None:
        stale_extra = {
            "codex_5h_used_percent": 10,
            "codex_7d_used_percent": 20,
            "codex_usage_updated_at": NOW.isoformat(),
        }
        row = oauth_row(extra=stale_extra)
        empty = {"success": True, "queried_at": NOW.isoformat(), "data": {}}
        helper = oauth_quota_summary_from_result(row, empty)
        used = [item.get("used_percent") for item in helper.get("ui_windows") or []]
        self.assertIn(10, used)
        self.assertIn(20, used)
        self.assertEqual(classify_oauth_account(row, empty, NOW, FRESHNESS), UNKNOWN)
        self.assertEqual(
            classify_oauth_account(
                row,
                {"success": True, "queried_at": NOW.isoformat()},
                NOW,
                FRESHNESS,
            ),
            UNKNOWN,
        )
        self.assertEqual(
            classify_oauth_account(
                row,
                {
                    "success": True,
                    "queried_at": NOW.isoformat(),
                    "data": "<html>nope</html>",
                    "oauth_quota": "bad",
                },
                NOW,
                FRESHNESS,
            ),
            UNKNOWN,
        )

    def test_fresh_usage_data_windows_still_classify_available(self) -> None:
        result = {
            "success": True,
            "queried_at": NOW.isoformat(),
            "data": {
                "five_hour": {"utilization": 20},
                "seven_day": {"utilization": 30},
            },
        }
        self.assertEqual(classify_oauth_account(oauth_row(), result, NOW, FRESHNESS), AVAILABLE)


class KeyFallbackConfigTests(unittest.TestCase):
    def test_default_missing_file_and_restart_reload(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controller, _monitor, _calls = make_controller(root, key_rows=[key_row(4)])
            loaded = controller.load_config()
            self.assertTrue(loaded.valid)
            self.assertFalse(loaded.enabled)
            self.assertEqual(loaded.managed_account_ids, ())
            saved = controller.save_user_config(enabled=True, managed_account_ids=["4"], user="admin")
            restarted, _monitor, _calls = make_controller(root, key_rows=[key_row(4)])
            again = restarted.load_config()
            self.assertTrue(again.enabled)
            self.assertEqual(again.managed_account_ids, (4,))
            self.assertEqual(again.config_version, saved.config_version)
            mode = stat.S_IMODE(Path(fallback_settings(root).key_fallback_config_path).stat().st_mode)
            leftovers = list(root.glob(".key-fallback-config.json.*.tmp"))
        self.assertEqual(mode, 0o600)
        self.assertEqual(leftovers, [])

    def test_save_rejects_stale_non_apikey_and_invalid_ids(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controller, _monitor, _calls = make_controller(
                root,
                key_rows=[key_row(4), key_row(8, platform="grok"), key_row(9, type="oauth")],
            )
            with self.assertRaises(KeyFallbackConfigError):
                controller.save_user_config(enabled=True, managed_account_ids=[4, 99], user="admin")
            with self.assertRaises(KeyFallbackConfigError):
                controller.save_user_config(enabled=True, managed_account_ids=[8], user="admin")
            with self.assertRaises(KeyFallbackConfigError):
                controller.save_user_config(enabled=True, managed_account_ids=[9], user="admin")
            with self.assertRaises(KeyFallbackConfigError):
                controller.save_user_config(enabled=True, managed_account_ids=["0", "-3"], user="admin")
            self.assertFalse(Path(fallback_settings(root).key_fallback_config_path).exists())

    def test_settings_path_is_configurable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "custom.json"
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "KEY_FALLBACK_CONFIG_PATH": str(path),
                },
                clear=True,
            ):
                loaded = load_settings()
        self.assertEqual(loaded.key_fallback_config_path, str(path))

    def test_invalid_persisted_ids_disable_policy_and_do_not_retarget(self) -> None:
        cases = (
            [1.9],
            [1.0],
            [True],
            [{"id": 4}],
            [4, {"id": 5}],
            [0],
            [-3],
            [None],
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controller, _monitor, calls = make_controller(
                root,
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4, schedulable=False)],
                snapshot={"oauth_results": {"1": failure_result("http_401")}},
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            path = Path(fallback_settings(root).key_fallback_config_path)
            mode = stat.S_IMODE(path.stat().st_mode)
            for invalid_ids in cases:
                payload = json.loads(path.read_text(encoding="utf-8"))
                payload["enabled"] = True
                payload["managed_account_ids"] = invalid_ids
                path.write_text(json.dumps(payload), encoding="utf-8")
                loaded = controller.load_config()
                report = controller.run_once(now=NOW)
                self.assertFalse(loaded.valid)
                self.assertEqual(loaded.managed_account_ids, ())
                self.assertEqual(report["reason"], "invalid_config")
            leftovers = list(root.glob(".key-fallback-config.json.*.tmp"))
            raw_invalid = (
                '{"enabled": true, "managed_account_ids": [Infinity], "config_version": 1}\n',
                '{"enabled": true, "managed_account_ids": [NaN], "config_version": 1}\n',
            )
            for raw in raw_invalid:
                path.write_text(raw, encoding="utf-8")
                loaded = controller.load_config()
                report = controller.run_once(now=NOW)
                self.assertFalse(loaded.valid)
                self.assertEqual(loaded.managed_account_ids, ())
                self.assertEqual(report["reason"], "invalid_config")
        self.assertEqual(calls, [])
        self.assertEqual(mode, 0o600)
        self.assertEqual(leftovers, [])

    def test_digit_string_ids_remain_valid(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controller, _monitor, _calls = make_controller(root, key_rows=[key_row(4)])
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            path = Path(fallback_settings(root).key_fallback_config_path)
            payload = json.loads(path.read_text(encoding="utf-8"))
            payload["managed_account_ids"] = ["4"]
            path.write_text(json.dumps(payload), encoding="utf-8")
            loaded = controller.load_config()
        self.assertTrue(loaded.valid)
        self.assertEqual(loaded.managed_account_ids, (4,))


class KeyFallbackControllerTests(unittest.TestCase):
    def test_all_unavailable_turns_selected_keys_on_and_skips_others(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        keys = [key_row(4, schedulable=False), key_row(7, schedulable=True), key_row(9, schedulable=False)]
        with tempfile.TemporaryDirectory() as directory:
            controller, monitor, calls = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=keys,
                snapshot=snapshot,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4, 7], user="admin")
            with patch("app.account_ops.pause_account") as pause, patch(
                "app.usage_query.execute_oauth_usage_query", wraps=execute_oauth_usage_query
            ) as query:
                report = controller.run_once(now=NOW)
            store = getattr(monitor, "snapshot")
        self.assertEqual(report["desired"], True)
        self.assertEqual(calls, [(4, True)])
        self.assertEqual(report["changed_ids"], [4])
        pause.assert_not_called()
        query.assert_not_called()
        self.assertEqual(monitor.calls, 1)
        self.assertIsInstance(store, dict)

    def test_any_available_turns_selected_keys_off(self) -> None:
        snapshot = {
            "oauth_results": {
                "1": success_result(),
                "2": failure_result("http_401"),
            }
        }
        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, calls = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1), oauth_row(2, schedulable=False)],
                key_rows=[key_row(4, schedulable=True)],
                snapshot=snapshot,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            report = controller.run_once(now=NOW)
        self.assertEqual(report["desired"], False)
        self.assertEqual(calls, [(4, False)])

    def test_mixed_unknown_empty_inventory_and_failures_keep_state(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("network_error")}}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mixed, monitor, calls = make_controller(
                root,
                oauth_rows=[oauth_row(1, schedulable=False), oauth_row(2)],
                key_rows=[key_row(4, schedulable=False)],
                snapshot=snapshot,
            )
            mixed.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            mixed_report = mixed.run_once(now=NOW)
            empty, _monitor, empty_calls = make_controller(
                root,
                oauth_rows=[],
                key_rows=[key_row(4, schedulable=False)],
                snapshot={"oauth_results": {}},
            )
            empty.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            empty_report = empty.run_once(now=NOW)
            failing, _monitor, fail_calls = make_controller(
                root,
                key_rows=[key_row(4)],
                snapshot=snapshot,
            )
            failing.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            failing.oauth_inventory = lambda _db: (_ for _ in ()).throw(RuntimeError("db down"))
            fail_report = failing.run_once(now=NOW)
            busy, busy_monitor, busy_calls = make_controller(
                root,
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4)],
                snapshot=snapshot,
                busy=True,
            )
            busy.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            busy_report = busy.run_once(now=NOW)
            disabled, disabled_monitor, disabled_calls = make_controller(
                root,
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4)],
                snapshot={"oauth_results": {"1": failure_result("http_401")}},
            )
            disabled.save_user_config(enabled=False, managed_account_ids=[4], user="admin")
            disabled_report = disabled.run_once(now=NOW)
        self.assertEqual(mixed_report["reason"], "keep_existing")
        self.assertEqual(calls, [])
        self.assertEqual(empty_report["reason"], "keep_existing")
        self.assertEqual(empty_calls, [])
        self.assertEqual(fail_report["reason"], "inventory_failed")
        self.assertEqual(fail_calls, [])
        self.assertEqual(busy_report["reason"], "refresh_in_progress")
        self.assertEqual(busy_calls, [])
        self.assertEqual(busy_monitor.calls, 1)
        self.assertEqual(disabled_report["reason"], "disabled_or_empty")
        self.assertEqual(disabled_monitor.calls, 0)
        self.assertEqual(disabled_calls, [])
        self.assertGreaterEqual(monitor.calls, 1)

    def test_noop_partial_failure_retry_and_boundary_filters(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        keys = {
            4: key_row(4, schedulable=False),
            7: key_row(7, schedulable=False),
            9: key_row(9, schedulable=False, platform="grok"),
            11: key_row(11, schedulable=False, type="oauth"),
        }
        calls: list[int] = []

        def runner(account_id: int, schedulable: bool, **_kwargs: object) -> dict[str, object]:
            calls.append(int(account_id))
            if account_id == 4:
                return {"success": False, "error_code": "http_500"}
            keys[account_id]["schedulable"] = schedulable
            return {"success": True}

        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, _ignored = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False), oauth_row(2, platform="grok")],
                key_rows=list(keys.values()),
                snapshot=snapshot,
                runner=runner,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4, 7], user="admin")
            config_path = Path(directory) / "key-fallback-config.json"
            payload = json.loads(config_path.read_text(encoding="utf-8"))
            payload["managed_account_ids"] = [4, 7, 9, 11, 99]
            config_path.write_text(json.dumps(payload), encoding="utf-8")
            first = controller.run_once(now=NOW)
            second = controller.run_once(now=NOW)
        self.assertEqual(first["changed_ids"], [7])
        self.assertEqual(first["failed_ids"], [4])
        self.assertEqual(calls, [4, 7, 4])
        self.assertEqual(second["changed_ids"], [])
        self.assertEqual(second["failed_ids"], [4])
        self.assertNotIn(9, calls)
        self.assertNotIn(11, calls)
        self.assertNotIn(99, calls)

    def test_night_does_not_block_key_switching_or_recovery_state(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        with tempfile.TemporaryDirectory() as directory, patch(
            "app.account_ops.pause_account"
        ) as pause, patch("app.oauth_monitor.execute_oauth_usage_query") as usage:
            controller, monitor, calls = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4, schedulable=False)],
                snapshot=snapshot,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            report = controller.run_once(now=NIGHT)
        self.assertEqual(report["desired"], True)
        self.assertEqual(calls, [(4, True)])
        pause.assert_not_called()
        usage.assert_not_called()
        self.assertEqual(monitor.calls, 1)

    def test_disable_or_remove_does_not_undo_schedulable(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, calls = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4, schedulable=True)],
                snapshot=snapshot,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            controller.run_once(now=NOW)
            controller.save_user_config(enabled=False, managed_account_ids=[4], user="admin")
            disabled = controller.run_once(now=NOW)
            controller.save_user_config(enabled=True, managed_account_ids=[], user="admin")
            emptied = controller.run_once(now=NOW)
        self.assertEqual(calls, [])
        self.assertEqual(disabled["reason"], "disabled_or_empty")
        self.assertEqual(emptied["reason"], "disabled_or_empty")

    def test_reenable_evaluates_afresh(self) -> None:
        snapshot = {"oauth_results": {"1": success_result()}}
        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, calls = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1)],
                key_rows=[key_row(4, schedulable=True)],
                snapshot=snapshot,
            )
            controller.save_user_config(enabled=False, managed_account_ids=[4], user="admin")
            controller.run_once(now=NOW)
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            report = controller.run_once(now=NOW)
        self.assertEqual(report["desired"], False)
        self.assertEqual(calls, [(4, False)])

    def test_revalidates_live_key_immediately_before_each_dispatch(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        keys = {
            4: key_row(4, schedulable=False),
            5: key_row(5, schedulable=False),
            6: key_row(6, schedulable=False),
            7: key_row(7, schedulable=False),
        }
        calls: list[int] = []

        def reader(_db: object, account_id: int) -> dict[str, object] | None:
            row = keys.get(int(account_id))
            return dict(row) if row else None

        def runner(account_id: int, schedulable: bool, **_kwargs: object) -> dict[str, object]:
            del schedulable
            calls.append(int(account_id))
            if account_id == 4:
                keys.pop(5, None)
                keys[6] = key_row(6, schedulable=False, type="oauth")
                keys[7]["schedulable"] = True
            return {"success": True}

        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, _ignored = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=list(keys.values()),
                snapshot=snapshot,
                runner=runner,
                account_reader=reader,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4, 5, 6, 7], user="admin")
            report = controller.run_once(now=NOW)
        self.assertEqual(report["desired"], True)
        self.assertEqual(calls, [4])
        self.assertEqual(report["changed_ids"], [4])

    def test_dispatch_budget_stops_new_writes_and_retries_fairly(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        keys = {
            4: key_row(4, schedulable=False),
            5: key_row(5, schedulable=False),
            6: key_row(6, schedulable=False),
            7: key_row(7, schedulable=False),
            9: key_row(9, schedulable=True),
        }
        clock = {"now": 1000.0}
        calls: list[tuple[int, object]] = []

        def monotonic() -> float:
            return float(clock["now"])

        def reader(_db: object, account_id: int) -> dict[str, object] | None:
            row = keys.get(int(account_id))
            return dict(row) if row else None

        def runner(account_id: int, schedulable: bool, **kwargs: object) -> dict[str, object]:
            del schedulable
            calls.append((int(account_id), kwargs.get("timeout_seconds")))
            clock["now"] += 4.0
            keys[int(account_id)]["schedulable"] = True
            return {"success": True}

        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, _ignored = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=list(keys.values()),
                snapshot=snapshot,
                runner=runner,
                account_reader=reader,
                monotonic=monotonic,
            )
            controller.save_user_config(
                enabled=True,
                managed_account_ids=[4, 5, 9, 6, 7],
                user="admin",
            )
            first = controller.run_once(now=NOW)
            second = controller.run_once(now=NOW)
        self.assertEqual(DISPATCH_BUDGET_SECONDS, 10)
        self.assertEqual(SCHEDULABLE_REQUEST_TIMEOUT_SECONDS, 3)
        self.assertEqual(first["reason"], "dispatch_budget_exhausted")
        self.assertEqual(first["changed_ids"], [4, 5, 6])
        self.assertEqual(second["changed_ids"], [7])
        self.assertEqual([item[0] for item in calls], [4, 5, 6, 7])
        self.assertEqual({item[1] for item in calls}, {3})
        self.assertNotIn(9, [item[0] for item in calls])


class KeyFallbackOwnershipTests(unittest.IsolatedAsyncioTestCase):
    def _bot_settings(self, directory: str) -> SimpleNamespace:
        return SimpleNamespace(
            telegram_enabled=True,
            telegram_bot_token="token",
            telegram_poll_timeout_seconds=5,
            telegram_pairing_enabled=True,
            telegram_pairing_code="ABCD-EFGH",
            telegram_allowed_chat_ids=(),
            telegram_allowed_user_ids=(),
            telegram_state_path=str(Path(directory) / "telegram-state.json"),
            usage_query_state_path=str(Path(directory) / "usage.json"),
            audit_path=str(Path(directory) / "audit.jsonl"),
        )

    async def test_manual_actions_unmanage_before_pause_and_abort_on_save_failure(self) -> None:
        row = key_row(4, schedulable=True)
        row.update({"temp_unschedulable_until": None, "temp_unschedulable_reason": None})

        class TrackingController:
            def __init__(self) -> None:
                self.order: list[str] = []
                self.fail = False

            def run_manual_account_action(
                self,
                account_id: int,
                action: str,
                *,
                actor_name: str,
                minutes: int | None = None,
                reason: str | None = None,
            ) -> dict[str, object] | None:
                del actor_name, minutes, reason
                self.order.append(f"release:{account_id}")
                if self.fail:
                    raise KeyFallbackConfigError("磁盘写入失败")
                self.order.append(f"{action}:{account_id}")
                if action == "pause":
                    return account_ops.pause_account(object(), "audit", account_id, "tg", "pause")
                if action == "resume":
                    return account_ops.resume_account(object(), "audit", account_id, "tg")
                return account_ops.cooldown_account(object(), "audit", account_id, "tg", 15, "cd")

        with tempfile.TemporaryDirectory() as directory:
            tracker = TrackingController()
            bot = TelegramOpsBot(
                self._bot_settings(directory),
                SimpleNamespace(
                    fetch_all=lambda *_a, **_k: [row],
                    fetch_one=lambda *_a, **_k: dict(row),
                ),
                key_fallback=tracker,
            )
            with (
                patch("app.account_ops.pause_account", return_value=row) as pause,
                patch("app.account_ops.cooldown_account", return_value=row) as cooldown,
                patch("app.account_ops.resume_account", return_value=row) as resume,
            ):
                pause_text, _ = await bot._callback_reply(1, 2, "pause:4")
                cooldown_text, _ = await bot._callback_reply(1, 2, "cd:4:15")
                resume_text, _ = await bot._callback_reply(1, 2, "res:4")
                shortcut, _ = await bot._text_reply("/account 4")
                tracker.fail = True
                paused_calls = pause.call_count
                abort_text, abort_keyboard = await bot._callback_reply(1, 2, "pause:4")

                class BoomController:
                    def run_manual_account_action(self, *_args: object, **_kwargs: object) -> None:
                        raise RuntimeError("token=secret-value")

                bot.key_fallback = BoomController()
                boom_text, boom_keyboard = await bot._callback_reply(1, 2, "pause:4")

        self.assertIn("已暂停", pause_text)
        self.assertIn("已冷却", cooldown_text)
        self.assertIn("已恢复", resume_text)
        self.assertIn("#4", shortcut)
        self.assertEqual(
            tracker.order[:6],
            ["release:4", "pause:4", "release:4", "cooldown:4", "release:4", "resume:4"],
        )
        self.assertEqual(abort_text, "操作已中止：无法更新 Key 回退配置。")
        self.assertNotIn("磁盘写入失败", abort_text)
        self.assertIsNone(abort_keyboard)
        self.assertEqual(boom_text, "操作已中止：账号操作失败。")
        self.assertNotIn("secret-value", boom_text)
        self.assertIsNone(boom_keyboard)
        self.assertEqual(pause.call_count, paused_calls)
        self.assertEqual(cooldown.call_count, 1)
        self.assertEqual(resume.call_count, 1)

    async def test_manual_action_without_controller_still_runs(self) -> None:
        row = key_row(4, schedulable=True)
        row.update({"temp_unschedulable_until": None, "temp_unschedulable_reason": None})
        with tempfile.TemporaryDirectory() as directory:
            bot = TelegramOpsBot(
                self._bot_settings(directory),
                SimpleNamespace(
                    fetch_all=lambda *_a, **_k: [row],
                    fetch_one=lambda *_a, **_k: dict(row),
                ),
                key_fallback=None,
            )
            with patch("app.account_ops.pause_account", return_value=row) as pause:
                text, _ = await bot._callback_reply(1, 2, "pause:4")
        self.assertIn("已暂停", text)
        pause.assert_called_once()

    def test_save_cannot_interleave_unmanage_and_manual_action(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        pause_started = threading.Event()
        pause_proceed = threading.Event()
        save_done = threading.Event()
        order: list[str] = []
        row = key_row(4, schedulable=True)

        def pause_account(*_args: object, **_kwargs: object) -> dict[str, object]:
            order.append("pause")
            pause_started.set()
            pause_proceed.wait(2)
            return row

        with tempfile.TemporaryDirectory() as directory, patch(
            "app.key_fallback.account_ops.pause_account",
            side_effect=pause_account,
        ) as pause, patch(
            "app.key_fallback.account_ops.cooldown_account",
            return_value=row,
        ) as cooldown, patch(
            "app.key_fallback.account_ops.resume_account",
            return_value=row,
        ) as resume:
            controller, _monitor, _calls = make_controller(
                Path(directory),
                key_rows=[key_row(4), key_row(7)],
                snapshot=snapshot,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")

            def manual() -> None:
                controller.run_manual_account_action(
                    4,
                    "pause",
                    actor_name="tg",
                    reason="telegram pause by tg",
                )
                order.append("manual-done")

            def save() -> None:
                pause_started.wait(2)
                controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
                order.append("save-done")
                save_done.set()

            worker = threading.Thread(target=manual)
            saver = threading.Thread(target=save)
            worker.start()
            self.assertTrue(pause_started.wait(2))
            saver.start()
            self.assertFalse(save_done.wait(0.05))
            self.assertNotIn("save-done", order)
            persisted = json.loads(
                (Path(directory) / "key-fallback-config.json").read_text(encoding="utf-8")
            )
            self.assertEqual(persisted["managed_account_ids"], [])
            pause_proceed.set()
            worker.join(timeout=2)
            saver.join(timeout=2)
            self.assertEqual(order[:2], ["pause", "manual-done"])
            self.assertIn("save-done", order)
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            controller.run_manual_account_action(4, "cooldown", actor_name="tg", minutes=15)
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            controller.run_manual_account_action(4, "resume", actor_name="tg")
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            with patch("app.key_fallback._atomic_write_json", side_effect=OSError("disk")):
                with self.assertRaises(KeyFallbackConfigError):
                    controller.run_manual_account_action(4, "pause", actor_name="tg")
            self.assertEqual(controller.load_config().managed_account_ids, (4,))
        self.assertEqual(pause.call_count, 1)
        self.assertEqual(cooldown.call_count, 1)
        self.assertEqual(resume.call_count, 1)

    def test_release_keeps_remaining_keys_and_concurrency_is_serialized(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        started = threading.Event()
        proceed = threading.Event()
        calls: list[int] = []

        def slow_runner(account_id: int, schedulable: bool, **_kwargs: object) -> dict[str, object]:
            del schedulable
            calls.append(int(account_id))
            started.set()
            proceed.wait(1)
            return {"success": True}

        with tempfile.TemporaryDirectory() as directory:
            controller, _monitor, _ignored = make_controller(
                Path(directory),
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4, schedulable=False), key_row(7, schedulable=False)],
                snapshot=snapshot,
                runner=slow_runner,
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4, 7], user="admin")
            worker = threading.Thread(target=lambda: controller.run_once(now=NOW))
            worker.start()
            self.assertTrue(started.wait(1))
            released = threading.Event()

            def unmanage() -> None:
                controller.release_managed_account(4)
                released.set()

            waiter = threading.Thread(target=unmanage)
            waiter.start()
            self.assertFalse(released.wait(0.05))
            proceed.set()
            worker.join(timeout=2)
            waiter.join(timeout=2)
            config = controller.load_config()
        self.assertTrue(released.is_set())
        self.assertEqual(config.managed_account_ids, (7,))
        self.assertEqual(calls, [4, 7])


class KeyFallbackRouteTests(unittest.IsolatedAsyncioTestCase):
    async def test_route_requires_auth_and_rejects_stale_selection(self) -> None:
        paths = {getattr(route, "path", "") for route in main_module.app.routes}
        self.assertIn("/key-fallback/config", paths)
        route = next(
            item
            for item in main_module.app.routes
            if getattr(item, "path", "") == "/key-fallback/config"
        )
        names = [str(dep.call.__name__) for dep in route.dependant.dependencies]
        self.assertIn("require_auth", names)

        class FormRequest:
            async def form(self) -> FormData:
                return FormData([("enabled", "1"), ("managed_account_ids", "99")])

        original = main_module.key_fallback_controller
        original_settings = main_module.settings
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controller, _monitor, _calls = make_controller(root, key_rows=[key_row(4)])
            main_module.key_fallback_controller = controller
            main_module.settings = SimpleNamespace(base_path="/sub2ops", audit_path=str(root / "audit.jsonl"))
            try:
                response = await main_module.key_fallback_config_save(FormRequest(), "admin")
            finally:
                main_module.key_fallback_controller = original
                main_module.settings = original_settings
        self.assertEqual(response.status_code, 303)
        self.assertIn("不是有效的 OpenAI apikey", unquote(response.headers["location"]))
        self.assertFalse(Path(fallback_settings(root).key_fallback_config_path).exists())

    async def test_valid_save_persists_and_redirects(self) -> None:
        class FormRequest:
            async def form(self) -> FormData:
                return FormData(
                    [
                        ("enabled", "1"),
                        ("managed_account_ids", "4"),
                        ("managed_account_ids", "4"),
                    ]
                )

        original = main_module.key_fallback_controller
        original_settings = main_module.settings
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controller, _monitor, _calls = make_controller(root, key_rows=[key_row(4), key_row(7)])
            main_module.key_fallback_controller = controller
            main_module.settings = SimpleNamespace(base_path="/sub2ops")
            try:
                response = await main_module.key_fallback_config_save(FormRequest(), "admin")
                persisted = json.loads((root / "key-fallback-config.json").read_text(encoding="utf-8"))
            finally:
                main_module.key_fallback_controller = original
                main_module.settings = original_settings
        self.assertEqual(response.status_code, 303)
        self.assertIn("Key", response.headers["location"])
        self.assertTrue(persisted["enabled"])
        self.assertEqual(persisted["managed_account_ids"], [4])
        self.assertNotIn("secret-key-material", json.dumps(persisted))


class KeyFallbackTransportTests(unittest.TestCase):
    def test_loopback_sends_only_schedulable_json_to_admin_endpoint(self) -> None:
        with AdminCapture() as capture:
            result = execute_sub2api_set_schedulable(
                12,
                True,
                base_url=capture.url,
                admin_token="admin-test-token",
            )
        self.assertTrue(result["success"])
        self.assertEqual(len(capture.requests), 1)
        request = capture.requests[0]
        self.assertEqual(request["path"], "/api/v1/admin/accounts/12/schedulable")
        self.assertEqual(request["content_type"], "application/json")
        self.assertEqual(request["payload"], {"schedulable": True})
        self.assertEqual(set(request["payload"]), {"schedulable"})
        self.assertNotIn("pause", str(request["path"]))
        self.assertNotIn("resume", str(request["path"]))
        self.assertEqual(result.get("data", {}).get("id"), 12)
        self.assertEqual(result.get("data", {}).get("schedulable"), True)

    def test_http_success_requires_code0_and_matching_schedulable_envelope(self) -> None:
        cases = [
            (200, "<html>error</html>"),
            (200, "{not json"),
            (200, {"code": 1, "data": {"schedulable": True, "id": 12}}),
            (200, {"code": 0, "message": "ok"}),
            (200, {"code": 0, "data": {}}),
            (200, {"code": 0, "data": {"schedulable": False, "id": 12}}),
            (200, {"code": 0, "data": {"schedulable": True, "id": 99}}),
            (500, {"code": 0, "data": {"schedulable": True, "id": 12}}),
        ]
        for status, body in cases:
            with self.subTest(status=status, body=body), AdminCapture(responses=[(status, body)]) as capture:
                result = execute_sub2api_set_schedulable(
                    12,
                    True,
                    base_url=capture.url,
                    admin_token="admin-test-token",
                )
            self.assertFalse(result["success"])
            dumped = json.dumps(result)
            self.assertNotIn("<html>", dumped)
            self.assertNotIn("admin-test-token", dumped)
            self.assertNotIn("{not json", dumped)

    def test_controller_uses_real_schedulable_transport(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        with tempfile.TemporaryDirectory() as directory, AdminCapture() as capture:
            keys = [key_row(12, schedulable=False)]
            controller = KeyFallbackController(
                fallback_settings(Path(directory)),
                db=object(),
                oauth_monitor=SnapshotMonitor(snapshot),
                base_url_provider=lambda: capture.url,
                admin_token_provider=lambda: "admin-test-token",
                oauth_inventory=lambda _db: [oauth_row(1, schedulable=False)],
                key_inventory=lambda _db: list(keys),
                account_reader=lambda _db, account_id: next(
                    (dict(row) for row in keys if int(row["id"]) == int(account_id)),
                    None,
                ),
            )
            controller.save_user_config(enabled=True, managed_account_ids=[12], user="admin")
            report = controller.run_once(now=NOW)
        self.assertEqual(report["changed_ids"], [12])
        self.assertEqual(capture.requests[0]["path"], "/api/v1/admin/accounts/12/schedulable")
        self.assertEqual(capture.requests[0]["payload"], {"schedulable": True})


class KeyFallbackLatestAttemptTests(unittest.TestCase):
    def _run_refresh_then_fallback(
        self,
        root: Path,
        *,
        usage_payloads: list[dict[str, object]],
        prior: dict[str, object] | None,
        key: dict[str, object],
        refresh_now: datetime,
        eval_now: datetime,
        second_refresh_now: datetime | None = None,
    ) -> tuple[dict[str, object], list[tuple[int, bool]]]:
        write_monitor_state(
            root,
            oauth_results={"1": prior} if prior else {},
        )
        oauth = oauth_row(1)
        payloads = list(usage_payloads)

        def usage_runner(*_args: object, **_kwargs: object) -> dict[str, object]:
            if not payloads:
                raise AssertionError("unexpected extra usage probe")
            return dict(payloads.pop(0))

        monitor = OAuthMonitor(
            monitor_settings(root, recovery_enabled=False),
            db=SimpleNamespace(fetch_all=lambda *_a, **_k: [dict(oauth)], fetch_one=lambda *_a, **_k: dict(oauth)),
            base_url_provider=lambda: "http://127.0.0.1:9",
            usage_runner=usage_runner,
            **offline_monitor_kwargs(oauth),  # type: ignore[arg-type]
        )
        writes: list[tuple[int, bool]] = []

        def runner(account_id: int, schedulable: bool, **_kwargs: object) -> dict[str, object]:
            writes.append((int(account_id), bool(schedulable)))
            return {"success": True}

        controller, _ignored, _calls = make_controller(
            root,
            oauth_rows=[dict(oauth)],
            key_rows=[dict(key)],
            runner=runner,
            monitor=monitor,  # type: ignore[arg-type]
        )
        controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
        first = monitor.force_refresh(now=refresh_now)
        self.assertFalse(first.get("timed_out"))
        if second_refresh_now is not None:
            second = monitor.force_refresh(now=second_refresh_now)
            self.assertFalse(second.get("timed_out"))
        report = controller.run_once(now=eval_now)
        self.assertEqual(payloads, [])
        return report, writes

    def test_force_refresh_401_after_success_enables_fallback_key(self) -> None:
        prior = success_result(queried_at=NOW - timedelta(minutes=10))
        failure = {
            "template_type": "oauth",
            "success": False,
            "queried_at": NOW.isoformat(),
            "error_code": "http_401",
            "error": "dummy",
        }
        with tempfile.TemporaryDirectory() as directory:
            report, writes = self._run_refresh_then_fallback(
                Path(directory),
                usage_payloads=[failure],
                prior=prior,
                key=key_row(4, schedulable=True),
                refresh_now=NOW,
                eval_now=NOW,
            )
        self.assertEqual(report["oauth_states"], {1: UNAVAILABLE})
        self.assertEqual(report["desired"], True)
        self.assertEqual(writes, [])

    def test_force_refresh_402_without_prior_success_enables_selected_key(self) -> None:
        failure = {
            "template_type": "oauth",
            "success": False,
            "queried_at": NOW.isoformat(),
            "error_code": "http_402",
            "error": "dummy",
        }
        with tempfile.TemporaryDirectory() as directory:
            report, writes = self._run_refresh_then_fallback(
                Path(directory),
                usage_payloads=[failure],
                prior=None,
                key=key_row(4, schedulable=False),
                refresh_now=NOW,
                eval_now=NOW,
            )
        self.assertEqual(report["oauth_states"], {1: UNAVAILABLE})
        self.assertEqual(report["desired"], True)
        self.assertEqual(writes, [(4, True)])

    def test_force_refresh_timeout_network_and_500_after_success_keep_key_state(self) -> None:
        prior = success_result(queried_at=NOW - timedelta(minutes=10))
        for error_code in ("timeout", "network_error", "http_500"):
            failure = {
                "template_type": "oauth",
                "success": False,
                "queried_at": NOW.isoformat(),
                "error_code": error_code,
                "error": "dummy",
            }
            with tempfile.TemporaryDirectory() as directory:
                report, writes = self._run_refresh_then_fallback(
                    Path(directory),
                    usage_payloads=[failure],
                    prior=prior,
                    key=key_row(4, schedulable=True),
                    refresh_now=NOW,
                    eval_now=NOW,
                )
            self.assertEqual(report["oauth_states"], {1: UNKNOWN}, error_code)
            self.assertIsNone(report["desired"], error_code)
            self.assertEqual(writes, [], error_code)

    def test_force_refresh_success_after_failure_disables_selected_key(self) -> None:
        failure = {
            "template_type": "oauth",
            "success": False,
            "queried_at": NOW.isoformat(),
            "error_code": "http_401",
            "error": "dummy",
        }
        success = {
            "template_type": "oauth",
            "success": True,
            "queried_at": (NOW + timedelta(minutes=1)).isoformat(),
            "oauth_quota": success_result()["oauth_quota"],
        }
        with tempfile.TemporaryDirectory() as directory:
            report, writes = self._run_refresh_then_fallback(
                Path(directory),
                usage_payloads=[failure, success],
                prior=None,
                key=key_row(4, schedulable=True),
                refresh_now=NOW,
                second_refresh_now=NOW + timedelta(minutes=1),
                eval_now=NOW + timedelta(minutes=1),
            )
            scheduler = json.loads((Path(directory) / "usage-query-state.json").read_text(encoding="utf-8"))
        self.assertTrue(scheduler["scheduler"]["1"].get("last_error_at"))
        self.assertEqual(report["oauth_states"], {1: AVAILABLE})
        self.assertEqual(report["desired"], False)
        self.assertEqual(writes, [(4, False)])

    def test_stale_force_refresh_failure_is_unknown(self) -> None:
        failure = {
            "template_type": "oauth",
            "success": False,
            "queried_at": NOW.isoformat(),
            "error_code": "http_401",
            "error": "dummy",
        }
        with tempfile.TemporaryDirectory() as directory:
            report, writes = self._run_refresh_then_fallback(
                Path(directory),
                usage_payloads=[failure],
                prior=None,
                key=key_row(4, schedulable=False),
                refresh_now=NOW,
                eval_now=NOW + timedelta(hours=2),
            )
        self.assertEqual(report["oauth_states"], {1: UNKNOWN})
        self.assertEqual(writes, [])


class OAuthMonitorSnapshotTests(unittest.TestCase):
    def test_committed_snapshot_skips_in_progress_cycle(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            monitor = OAuthMonitor(
                SimpleNamespace(
                    usage_query_state_path=str(path),
                    audit_path=str(Path(directory) / "audit.jsonl"),
                    telegram_oauth_usage_refresh_enabled=True,
                    telegram_oauth_recovery_monitor_enabled=True,
                    telegram_oauth_night_recovery_cooldown_enabled=True,
                    telegram_oauth_usage_refresh_concurrency=1,
                    telegram_oauth_recovery_test_concurrency=1,
                    telegram_oauth_early_probe_batch_size=8,
                    telegram_oauth_regular_refresh_interval_seconds=3600,
                    telegram_oauth_7d_probe_interval_seconds=3600,
                    telegram_oauth_recovery_test_model_id="gpt-5.6-luna",
                ),
                db=SimpleNamespace(fetch_all=lambda *_a, **_k: []),
                base_url_provider=lambda: "http://127.0.0.1:9",
                **offline_monitor_kwargs(),  # type: ignore[arg-type]
            )
            snapshot = monitor.committed_snapshot()
            self.assertIsInstance(snapshot, dict)
            self.assertEqual(snapshot.get("version"), 3)
            monitor._run_lock.acquire()
            try:
                self.assertIsNone(monitor.committed_snapshot())
                with monitor.evaluation_guard() as session:
                    self.assertIsNone(session)
            finally:
                monitor._run_lock.release()

    def test_evaluation_guard_blocks_monitor_until_fallback_releases(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        snapshot_taken = threading.Event()
        release_write = threading.Event()
        cycle_started = threading.Event()
        usage_calls: list[str] = []

        def slow_runner(account_id: int, schedulable: bool, **_kwargs: object) -> dict[str, object]:
            del account_id, schedulable
            snapshot_taken.set()
            release_write.wait(2)
            return {"success": True}

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            monitor = OAuthMonitor(
                SimpleNamespace(
                    usage_query_state_path=str(root / "state.json"),
                    audit_path=str(root / "audit.jsonl"),
                    telegram_oauth_usage_refresh_enabled=True,
                    telegram_oauth_recovery_monitor_enabled=True,
                    telegram_oauth_night_recovery_cooldown_enabled=True,
                    telegram_oauth_usage_refresh_concurrency=1,
                    telegram_oauth_recovery_test_concurrency=1,
                    telegram_oauth_early_probe_batch_size=8,
                    telegram_oauth_regular_refresh_interval_seconds=3600,
                    telegram_oauth_7d_probe_interval_seconds=3600,
                    telegram_oauth_recovery_test_model_id="gpt-5.6-luna",
                ),
                db=SimpleNamespace(fetch_all=lambda *_a, **_k: []),
                base_url_provider=lambda: "http://127.0.0.1:9",
                inventory_loader=lambda _db: [],
                usage_runner=lambda *_a, **_k: usage_calls.append("usage") or {},
                test_runner=lambda *_a, **_k: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_a, **_k: {"success": True, "method": "recover-state"},
            )
            original_cycle = monitor._run_cycle

            def wrapped_cycle(*args: object, **kwargs: object) -> object:
                cycle_started.set()
                return original_cycle(*args, **kwargs)

            monitor._run_cycle = wrapped_cycle  # type: ignore[method-assign]
            controller, _ignored_monitor, _calls = make_controller(
                root,
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4, schedulable=False)],
                snapshot=snapshot,
                runner=slow_runner,
                monitor=monitor,  # type: ignore[arg-type]
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            worker = threading.Thread(target=lambda: controller.run_once(now=NOW))
            worker.start()
            self.assertTrue(snapshot_taken.wait(2))
            monitor.run_once(now=NOW)
            self.assertFalse(cycle_started.is_set())
            self.assertEqual(usage_calls, [])
            release_write.set()
            worker.join(timeout=2)
            monitor.run_once(now=NOW)
        self.assertTrue(cycle_started.is_set())
        self.assertEqual(usage_calls, [])

    def test_fallback_does_not_write_during_active_recovery(self) -> None:
        snapshot = {"oauth_results": {"1": failure_result("http_401")}}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            monitor = OAuthMonitor(
                SimpleNamespace(
                    usage_query_state_path=str(root / "state.json"),
                    audit_path=str(root / "audit.jsonl"),
                    telegram_oauth_usage_refresh_enabled=True,
                    telegram_oauth_recovery_monitor_enabled=True,
                    telegram_oauth_night_recovery_cooldown_enabled=True,
                    telegram_oauth_usage_refresh_concurrency=1,
                    telegram_oauth_recovery_test_concurrency=1,
                    telegram_oauth_early_probe_batch_size=8,
                    telegram_oauth_regular_refresh_interval_seconds=3600,
                    telegram_oauth_7d_probe_interval_seconds=3600,
                    telegram_oauth_recovery_test_model_id="gpt-5.6-luna",
                ),
                db=SimpleNamespace(fetch_all=lambda *_a, **_k: []),
                base_url_provider=lambda: "http://127.0.0.1:9",
                inventory_loader=lambda _db: [],
                usage_runner=lambda *_a, **_k: (_ for _ in ()).throw(RuntimeError("usage probe")),
                test_runner=lambda *_a, **_k: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_a, **_k: {"success": True, "method": "recover-state"},
            )
            controller, _ignored, calls = make_controller(
                root,
                oauth_rows=[oauth_row(1, schedulable=False)],
                key_rows=[key_row(4, schedulable=False)],
                snapshot=snapshot,
                monitor=monitor,  # type: ignore[arg-type]
            )
            controller.save_user_config(enabled=True, managed_account_ids=[4], user="admin")
            monitor._run_lock.acquire()
            try:
                with patch("app.usage_query.execute_oauth_usage_query") as query:
                    report = controller.run_once(now=NOW)
            finally:
                monitor._run_lock.release()
        self.assertEqual(report["reason"], "refresh_in_progress")
        self.assertEqual(calls, [])
        query.assert_not_called()


if __name__ == "__main__":
    unittest.main()
