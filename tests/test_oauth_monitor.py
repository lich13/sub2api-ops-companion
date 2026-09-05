from __future__ import annotations

import io
import json
import tempfile
import threading
import unittest
import urllib.error
from collections.abc import Callable
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from app.oauth_monitor import (
    OAuthMonitor,
    OAuthStateStore,
    _retry_at,
    account_recovery_confirmed,
    automatic_recovery_eligible,
    beijing_cooldown_end,
    build_monitor_candidates,
    execute_sub2api_account_recovery,
    in_beijing_night_cooldown,
    migrate_legacy_recovery_state,
    recovery_block_change_is_safe,
    recovery_block_signature,
)

NOW = datetime(2026, 1, 10, 8, 0, tzinfo=timezone.utc)


def window(key: str, used: float, reset_at: datetime) -> dict[str, object]:
    return {
        "key": key,
        "label": "5h" if key == "codex_5h" else "7d",
        "used_percent": used,
        "remaining_percent": max(0, 100 - used),
        "depleted": used >= 100,
        "reset_at": reset_at.isoformat(),
    }


def summary(
    *,
    plan: str = "plus",
    five_used: float = 20,
    seven_used: float = 30,
    five_reset: datetime | None = None,
    seven_reset: datetime | None = None,
) -> dict[str, object]:
    windows = []
    if plan != "free":
        windows.append(window("codex_5h", five_used, five_reset or NOW + timedelta(hours=1)))
    windows.append(window("codex_7d", seven_used, seven_reset or NOW + timedelta(days=1)))
    return {
        "plan_type": plan,
        "ui_windows": windows,
        "telegram_windows": [item for item in windows if float(item["used_percent"]) < 100],
    }


def result(payload: dict[str, object], queried_at: datetime = NOW) -> dict[str, object]:
    return {
        "account_id": 1,
        "template_type": "oauth",
        "success": True,
        "queried_at": queried_at.isoformat(),
        "oauth_quota": payload,
        "source": "sub2api_admin_usage",
    }


def account(account_id: int = 1, *, plan: str = "plus") -> dict[str, object]:
    return {
        "id": account_id,
        "name": f"account-{account_id}",
        "platform": "openai",
        "type": "oauth",
        "credentials": {"plan_type": plan},
        "extra": {},
        "status": "active",
        "schedulable": True,
        "concurrency": 1,
        "updated_at": (NOW - timedelta(minutes=1)).isoformat(),
        "rate_limited_at": (NOW - timedelta(minutes=5)).isoformat(),
        "rate_limit_reset_at": NOW.isoformat(),
        "overload_until": None,
        "temp_unschedulable_until": None,
        "temp_unschedulable_reason": "",
        "error_message": "",
    }


def settings(path: Path) -> SimpleNamespace:
    return SimpleNamespace(
        usage_query_state_path=str(path),
        audit_path=str(path.with_name("audit.jsonl")),
        telegram_oauth_usage_refresh_enabled=True,
        telegram_oauth_recovery_monitor_enabled=True,
        telegram_oauth_night_recovery_cooldown_enabled=True,
        telegram_oauth_usage_refresh_concurrency=4,
        telegram_oauth_recovery_test_concurrency=2,
        telegram_oauth_early_probe_batch_size=8,
        telegram_oauth_regular_refresh_interval_seconds=3600,
        telegram_oauth_7d_probe_interval_seconds=3600,
        telegram_oauth_recovery_test_model_id="gpt-5.6-luna",
    )


class FakeDb:
    def __init__(self, rows: list[dict[str, object]] | None = None) -> None:
        self.rows = rows or []
        self.deleted = False

    def fetch_all(self, sql: str, params: dict[str, object] | None = None) -> list[dict[str, object]]:
        if "DELETE FROM scheduled_test_plans" in sql:
            self.deleted = True
            return [{"id": 9, "account_id": 3}]
        return list(self.rows)

    def fetch_one(self, _sql: str, params: dict[str, object] | None = None) -> dict[str, object] | None:
        account_id = int((params or {}).get("account_id") or 0)
        row = next((item for item in self.rows if int(item.get("id") or 0) == account_id), None)
        return dict(row) if row else None


class OAuthStateStoreTests(unittest.TestCase):
    def test_v2_testing_intent_migrates_to_retry_in_v3(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "usage-query-state.json"
            path.write_text(
                json.dumps(
                    {
                        "version": 2,
                        "scheduler": {
                            "1": {
                                "recovery_intent": {
                                    "fingerprint": "codex_7d@reset",
                                    "status": "testing",
                                    "tested_at": NOW.isoformat(),
                                }
                            }
                        },
                    }
                ),
                encoding="utf-8",
            )

            store = OAuthStateStore(str(path))
            self.assertTrue(store.migrate())

            persisted = json.loads(path.read_text(encoding="utf-8"))
            intent = persisted["scheduler"]["1"]["recovery_intent"]
            self.assertEqual(persisted["version"], 3)
            self.assertEqual(intent["status"], "retry")
            self.assertEqual(intent["next_retry_at"], NOW.isoformat())

    def test_migration_keeps_admin_key_and_oauth_snapshots_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "usage-query-state.json"
            path.write_text(
                json.dumps(
                    {
                        "settings": {
                            "sub2api_admin_token": "admin-secret",
                            "usage_query_enabled": True,
                            "auto_query_interval_seconds": 15,
                        },
                        "configs": {"1": {"template_type": "custom"}},
                        "results": {
                            "1": {"template_type": "custom", "success": True},
                            "2": result(summary()),
                        },
                    }
                ),
                encoding="utf-8",
            )

            store = OAuthStateStore(str(path))
            changed = store.migrate()
            persisted = json.loads(path.read_text(encoding="utf-8"))

            self.assertTrue(changed)
            self.assertEqual(store.admin_token(), "admin-secret")
            self.assertEqual(set(store.results()), {2})
            self.assertNotIn("configs", persisted)
            self.assertNotIn("usage_query_enabled", persisted["settings"])
            self.assertNotIn("auto_query_interval_seconds", persisted["settings"])

    def test_legacy_guard_cleanup_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            guard_path = root / "guard-state.json"
            guard_path.write_text('{"policy": {"endless_account_ids": [3]}}', encoding="utf-8")
            store = OAuthStateStore(str(state_path))
            db = FakeDb()

            first = migrate_legacy_recovery_state(db, store, str(guard_path), str(root / "audit.jsonl"))
            second = migrate_legacy_recovery_state(db, store, str(guard_path), str(root / "audit.jsonl"))

            self.assertTrue(first["success"])
            self.assertEqual(first["deleted_count"], 1)
            self.assertTrue(second["skipped"])
            self.assertFalse(guard_path.exists())

    def test_legacy_guard_cleanup_failure_keeps_state_for_retry(self) -> None:
        class BrokenDb(FakeDb):
            def fetch_all(self, sql: str, params: dict[str, object] | None = None) -> list[dict[str, object]]:
                raise RuntimeError("database unavailable")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            guard_path = root / "guard-state.json"
            guard_path.write_text("{}", encoding="utf-8")
            store = OAuthStateStore(str(state_path))

            outcome = migrate_legacy_recovery_state(
                BrokenDb(), store, str(guard_path), str(root / "audit.jsonl")
            )

            self.assertFalse(outcome["success"])
            self.assertTrue(guard_path.exists())
            self.assertFalse(store.legacy_recovery_cleanup_completed())


class OAuthMonitorSchedulingTests(unittest.TestCase):
    def test_regular_refresh_waits_one_hour(self) -> None:
        rows = [account()]
        results = {1: result(summary())}
        scheduler = {1: {"last_regular_at": NOW.isoformat()}}

        before = build_monitor_candidates(rows, results, scheduler, NOW + timedelta(seconds=3599))
        due = build_monitor_candidates(rows, results, scheduler, NOW + timedelta(seconds=3600))

        self.assertEqual(before, [])
        self.assertEqual([item["reason"] for item in due], ["regular_refresh"])

    def test_seven_day_probe_waits_one_hour(self) -> None:
        quota = summary(seven_used=100, seven_reset=NOW + timedelta(days=1))
        rows = [account()]
        results = {1: result(quota)}
        scheduler = {1: {"last_7d_probe_at": NOW.isoformat()}}

        before = build_monitor_candidates(rows, results, scheduler, NOW + timedelta(seconds=3599))
        due = build_monitor_candidates(rows, results, scheduler, NOW + timedelta(seconds=3600))

        self.assertEqual(before, [])
        self.assertEqual([item["reason"] for item in due], ["seven_day_probe"])

    def test_exact_reset_uses_latest_full_required_window(self) -> None:
        quota = summary(
            five_used=100,
            seven_used=100,
            five_reset=NOW + timedelta(seconds=10),
            seven_reset=NOW + timedelta(seconds=20),
        )
        rows = [account()]
        results = {1: result(quota)}

        early = build_monitor_candidates(rows, results, {}, NOW + timedelta(seconds=15))
        due = build_monitor_candidates(rows, results, {}, NOW + timedelta(seconds=20))

        self.assertEqual(early, [])
        self.assertEqual(len(due), 1)
        self.assertEqual(due[0]["reason"], "exact_reset")

    def test_one_account_is_only_scheduled_once(self) -> None:
        quota = summary(seven_used=100, seven_reset=NOW)
        candidates = build_monitor_candidates(
            [account()],
            {1: result(quota, NOW - timedelta(hours=2))},
            {1: {"last_7d_probe_at": (NOW - timedelta(hours=2)).isoformat()}},
            NOW,
        )

        self.assertEqual(len(candidates), 1)
        self.assertEqual(candidates[0]["reason"], "exact_reset")

    def test_candidates_are_ordered_by_recovery_priority(self) -> None:
        exact = summary(seven_used=100, seven_reset=NOW)
        probe = summary(seven_used=100, seven_reset=NOW + timedelta(days=1))
        regular = summary()
        rows = [account(3), account(2), account(1)]
        candidates = build_monitor_candidates(
            rows,
            {
                1: result(exact, NOW - timedelta(hours=2)),
                2: result(probe, NOW - timedelta(hours=2)),
                3: result(regular, NOW - timedelta(hours=2)),
            },
            {
                2: {"last_7d_probe_at": (NOW - timedelta(hours=2)).isoformat()},
                3: {"last_regular_at": (NOW - timedelta(hours=2)).isoformat()},
            },
            NOW,
        )

        self.assertEqual(
            [(item["account_id"], item["reason"]) for item in candidates],
            [(1, "exact_reset"), (2, "seven_day_probe"), (3, "regular_refresh")],
        )


class OAuthRecoveryGateTests(unittest.TestCase):
    def test_recovery_backoff_schedule_caps_at_30_minutes(self) -> None:
        self.assertEqual(_retry_at(NOW, 1), (NOW + timedelta(seconds=60)).isoformat())
        self.assertEqual(_retry_at(NOW, 2), (NOW + timedelta(minutes=5)).isoformat())
        self.assertEqual(_retry_at(NOW, 3), (NOW + timedelta(minutes=15)).isoformat())
        self.assertEqual(_retry_at(NOW, 4), (NOW + timedelta(minutes=30)).isoformat())
        self.assertEqual(_retry_at(NOW, 99), (NOW + timedelta(minutes=30)).isoformat())

    def test_beijing_night_boundaries_and_utc_date_crossover(self) -> None:
        cases = (
            (datetime(2026, 1, 10, 15, 59, tzinfo=timezone.utc), False),  # 23:59
            (datetime(2026, 1, 10, 16, 0, tzinfo=timezone.utc), True),    # 00:00
            (datetime(2026, 1, 10, 20, 59, tzinfo=timezone.utc), True),   # 04:59
            (datetime(2026, 1, 10, 21, 0, tzinfo=timezone.utc), False),   # 05:00
        )
        for instant, expected in cases:
            with self.subTest(instant=instant):
                self.assertEqual(in_beijing_night_cooldown(instant), expected)
        self.assertFalse(in_beijing_night_cooldown(cases[1][0], enabled=False))
        self.assertEqual(
            beijing_cooldown_end(cases[1][0]),
            datetime(2026, 1, 10, 21, 0, tzinfo=timezone.utc),
        )

    def test_automatic_recovery_is_strict_and_fail_closed(self) -> None:
        valid = account()
        self.assertTrue(
            automatic_recovery_eligible(valid, exhausted_window_keys=["codex_5h"], now=NOW)
        )
        mutations = (
            ("platform", "anthropic"),
            ("type", "apikey"),
            ("status", "error"),
            ("schedulable", False),
            ("deleted_at", NOW.isoformat()),
        )
        for key, value in mutations:
            with self.subTest(key=key):
                row = {**valid, key: value}
                self.assertFalse(
                    automatic_recovery_eligible(row, exhausted_window_keys=["codex_5h"], now=NOW)
                )

        threshold = {
            **valid,
            "rate_limited_at": None,
            "rate_limit_reset_at": None,
            "temp_unschedulable_reason": json.dumps(
                {
                    "source": "account_scheduling_threshold",
                    "platform": "openai",
                    "window": "7d",
                    "threshold_percent": 90,
                    "until_unix": int(NOW.timestamp()),
                    "error_message": "threshold reached",
                }
            ),
            "temp_unschedulable_until": NOW.isoformat(),
        }
        self.assertTrue(
            automatic_recovery_eligible(
                threshold, exhausted_window_keys=["codex_5h", "codex_7d"], now=NOW
            )
        )
        for reason in ("telegram cooldown", "manual pause", "unknown"):
            row = {**threshold, "temp_unschedulable_reason": reason}
            self.assertFalse(
                automatic_recovery_eligible(row, exhausted_window_keys=["codex_7d"], now=NOW)
            )
            rate_limited = {**valid, "temp_unschedulable_reason": reason}
            self.assertFalse(
                automatic_recovery_eligible(
                    rate_limited, exhausted_window_keys=["codex_7d"], now=NOW
                )
            )
        wrong_window = {**threshold, "temp_unschedulable_reason": threshold["temp_unschedulable_reason"].replace('"7d"', '"5h"')}
        self.assertFalse(
            automatic_recovery_eligible(
                wrong_window, exhausted_window_keys=["codex_7d"], now=NOW
            )
        )
        valid_payload = json.loads(str(threshold["temp_unschedulable_reason"]))
        malformed_payloads = []
        for missing in ("platform", "window", "threshold_percent", "until_unix"):
            payload = dict(valid_payload)
            payload.pop(missing)
            malformed_payloads.append(payload)
        malformed_payloads.extend(
            [
                {**valid_payload, "platform": "anthropic"},
                {**valid_payload, "window": "3h"},
                {**valid_payload, "threshold_percent": "bad"},
                {**valid_payload, "threshold_percent": 0},
                {**valid_payload, "threshold_percent": 101},
                {**valid_payload, "until_unix": "bad"},
            ]
        )
        for payload in malformed_payloads:
            with self.subTest(payload=payload):
                malformed = {
                    **threshold,
                    "temp_unschedulable_reason": json.dumps(payload),
                }
                self.assertFalse(
                    automatic_recovery_eligible(
                        malformed, exhausted_window_keys=["codex_7d"], now=NOW
                    )
                )
        mismatched_until = {
            **threshold,
            "temp_unschedulable_until": (NOW + timedelta(seconds=2)).isoformat(),
        }
        self.assertFalse(
            automatic_recovery_eligible(
                mismatched_until, exhausted_window_keys=["codex_7d"], now=NOW
            )
        )
        overload_only = {
            **valid,
            "rate_limited_at": None,
            "rate_limit_reset_at": None,
            "overload_until": (NOW + timedelta(minutes=5)).isoformat(),
        }
        self.assertFalse(
            automatic_recovery_eligible(
                overload_only, exhausted_window_keys=["codex_7d"], now=NOW
            )
        )

    def test_block_signature_detects_concurrency_change(self) -> None:
        row = account()
        changed = {**row, "concurrency": 2}
        self.assertNotEqual(recovery_block_signature(row), recovery_block_signature(changed))
        harmless_test_updates = {
            **row,
            "updated_at": (NOW + timedelta(seconds=1)).isoformat(),
            "error_message": "test completed",
        }
        self.assertEqual(
            recovery_block_signature(row), recovery_block_signature(harmless_test_updates)
        )
        partially_cleared = {**row, "rate_limited_at": None}
        self.assertTrue(recovery_block_change_is_safe(row, partially_cleared))
        self.assertFalse(recovery_block_change_is_safe(row, changed))
        replacement_block = {
            **row,
            "rate_limit_reset_at": (NOW + timedelta(hours=1)).isoformat(),
        }
        self.assertFalse(recovery_block_change_is_safe(row, replacement_block))
        cleared = {**row, "rate_limited_at": None, "rate_limit_reset_at": None}
        self.assertTrue(account_recovery_confirmed(cleared))


class OAuthRecoveryHttpTests(unittest.TestCase):
    class Response:
        status = 200

        def __enter__(self) -> OAuthRecoveryHttpTests.Response:
            return self

        def __exit__(self, *_args: object) -> None:
            return None

        def read(self) -> bytes:
            return b'{"code":0,"message":"success"}'

    def test_recover_state_success_does_not_call_fallback(self) -> None:
        calls: list[tuple[str, str]] = []

        def opener(request: object, timeout: int) -> object:
            calls.append((request.full_url, request.method))
            return self.Response()

        with patch("app.oauth_monitor.urllib.request.urlopen", side_effect=opener):
            result = execute_sub2api_account_recovery(
                7, base_url="https://sub2api.example.com", admin_token="key"
            )

        self.assertTrue(result["success"])
        self.assertEqual(result["method"], "recover-state")
        self.assertEqual(calls, [("https://sub2api.example.com/api/v1/admin/accounts/7/recover-state", "POST")])

    def test_recover_state_404_uses_both_official_fallbacks(self) -> None:
        calls: list[tuple[str, str]] = []

        def opener(request: object, timeout: int) -> object:
            calls.append((request.full_url, request.method))
            if len(calls) == 1:
                raise urllib.error.HTTPError(
                    request.full_url, 404, "not found", {}, io.BytesIO(b'{"reason":"ACCOUNT_NOT_FOUND"}')
                )
            return self.Response()

        with patch("app.oauth_monitor.urllib.request.urlopen", side_effect=opener):
            result = execute_sub2api_account_recovery(
                7, base_url="https://sub2api.example.com", admin_token="key"
            )

        self.assertTrue(result["success"])
        self.assertTrue(result["fallback"])
        self.assertEqual(
            [item[1] for item in calls],
            ["POST", "POST", "DELETE"],
        )
        self.assertTrue(calls[1][0].endswith("/clear-rate-limit"))
        self.assertTrue(calls[2][0].endswith("/temp-unschedulable"))

    def test_non_404_recovery_failure_never_falls_back(self) -> None:
        error = urllib.error.HTTPError(
            "https://sub2api.example.com/recover-state",
            500,
            "failed",
            {},
            io.BytesIO(b"failed"),
        )
        with patch("app.oauth_monitor.urllib.request.urlopen", side_effect=error) as opener:
            result = execute_sub2api_account_recovery(
                7, base_url="https://sub2api.example.com", admin_token="key"
            )
        self.assertFalse(result["success"])
        self.assertEqual(result["error_code"], "http_500")
        self.assertEqual(opener.call_count, 1)


class OAuthMonitorExecutionTests(unittest.TestCase):
    def make_monitor(
        self,
        root: Path,
        old_summary: dict[str, object],
        refreshed_summary: dict[str, object],
        *,
        usage_error: tuple[str, str] | None = None,
        test_success: bool = True,
        test_clears_block: bool = True,
        recovery_success: bool = True,
        mutate_after_test: Callable[[dict[str, object]], None] | None = None,
        initial_cache: bool = True,
    ) -> tuple[OAuthMonitor, dict[str, object]]:
        state_path = root / "usage-query-state.json"
        state_path.write_text(
            json.dumps(
                {
                    "version": 2,
                    "settings": {"sub2api_admin_token": "admin-key"},
                    "oauth_results": (
                        {"1": result(old_summary, NOW - timedelta(hours=2))}
                        if initial_cache
                        else {}
                    ),
                    "scheduler": {},
                    "pending_events": {},
                }
            ),
            encoding="utf-8",
        )
        calls: dict[str, object] = {"usage": 0, "test": 0, "recovery": 0, "models": []}
        database = FakeDb([account()])

        def usage_runner(account_id: int, *_args: object, **_kwargs: object) -> dict[str, object]:
            calls["usage"] = int(calls["usage"]) + 1
            if usage_error:
                return {
                    "account_id": account_id,
                    "template_type": "oauth",
                    "success": False,
                    "queried_at": NOW.isoformat(),
                    "error": usage_error[1],
                    "error_code": usage_error[0],
                }
            return result(refreshed_summary, NOW)

        def test_runner(account_id: int, model_id: str, **_kwargs: object) -> dict[str, object]:
            calls["test"] = int(calls["test"]) + 1
            cast_models = calls["models"]
            assert isinstance(cast_models, list)
            cast_models.append(model_id)
            if mutate_after_test is not None:
                mutate_after_test(database.rows[0])
            if test_success:
                if test_clears_block:
                    for row in database.rows:
                        if int(row.get("id") or 0) == account_id:
                            row["rate_limited_at"] = None
                            row["rate_limit_reset_at"] = None
                            row["updated_at"] = NOW.isoformat()
                return {"success": True, "model_id": model_id, "duration_ms": 18}
            return {
                "success": False,
                "model_id": model_id,
                "duration_ms": 18,
                "error_code": "http_502",
                "error": "upstream failed",
            }

        def recovery_runner(account_id: int, **_kwargs: object) -> dict[str, object]:
            calls["recovery"] = int(calls["recovery"]) + 1
            if recovery_success:
                for row in database.rows:
                    if int(row.get("id") or 0) == account_id:
                        row["rate_limited_at"] = None
                        row["rate_limit_reset_at"] = None
                        row["overload_until"] = None
                        row["temp_unschedulable_until"] = None
                        row["temp_unschedulable_reason"] = ""
                return {"success": True, "method": "recover-state"}
            return {"success": False, "error_code": "http_500", "error": "recover failed"}

        monitor = OAuthMonitor(
            settings(state_path),
            database,
            base_url_provider=lambda: "https://sub2api.example.com",
            inventory_loader=lambda _db: list(database.rows),
            usage_runner=usage_runner,
            test_runner=test_runner,
            recovery_runner=recovery_runner,
        )
        return monitor, calls

    def test_fresh_available_quota_recovers_existing_rate_limit_without_cache_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(
                Path(directory),
                summary(),
                summary(five_used=0, seven_used=0),
                initial_cache=False,
            )

            first = monitor.force_refresh(now=NOW)
            second = monitor.force_refresh(now=NOW + timedelta(seconds=1))

            self.assertEqual(first["recovered_count"], 1)
            self.assertEqual(second["recovered_count"], 0)
            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 0)
            intent = monitor.store.scheduler()[1]["recovery_intent"]
            self.assertEqual(intent["status"], "recovered")

    def test_depleted_seven_day_never_runs_account_test(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = summary(seven_used=100, seven_reset=NOW)
            refreshed = summary(five_used=0, seven_used=100, seven_reset=NOW + timedelta(days=7))
            monitor, calls = self.make_monitor(root, old, refreshed)

            events = monitor.run_once(NOW)

            self.assertEqual(calls["usage"], 1)
            self.assertEqual(calls["test"], 0)
            self.assertEqual(events, [])

    def test_idle_ticks_do_not_reread_database_or_state_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = summary()
            monitor, calls = self.make_monitor(root, current, current)
            monitor.store.update_scheduler({1: {"last_regular_at": NOW.isoformat()}})
            inventory_calls = 0
            original_loader = monitor.inventory_loader

            def inventory_loader(db: object) -> list[dict[str, object]]:
                nonlocal inventory_calls
                inventory_calls += 1
                return original_loader(db)

            monitor.inventory_loader = inventory_loader
            read_calls = 0
            original_read = monitor.store._read_raw

            def counted_read() -> dict[str, object]:
                nonlocal read_calls
                read_calls += 1
                return original_read()

            monitor.store._read_raw = counted_read  # type: ignore[method-assign]

            monitor.run_once(NOW)
            reads_after_first = read_calls
            monitor.run_once(NOW + timedelta(seconds=2))

            self.assertEqual(calls["usage"], 0)
            self.assertEqual(inventory_calls, 1)
            self.assertEqual(read_calls, reads_after_first)

    def test_bootstrap_queue_is_bounded_and_advances_between_ticks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            state_path.write_text(
                json.dumps(
                    {
                        "version": 2,
                        "settings": {"sub2api_admin_token": "admin-key"},
                        "oauth_results": {},
                        "scheduler": {},
                        "pending_events": {},
                    }
                ),
                encoding="utf-8",
            )
            monitor_settings = settings(state_path)
            monitor_settings.telegram_oauth_early_probe_batch_size = 2
            accounts = [account(value) for value in range(1, 6)]
            called: list[int] = []

            def usage_runner(account_id: int, *_args: object, **_kwargs: object) -> dict[str, object]:
                called.append(account_id)
                payload = result(summary(), NOW)
                payload["account_id"] = account_id
                return payload

            monitor = OAuthMonitor(
                monitor_settings,
                FakeDb(accounts),
                base_url_provider=lambda: "https://sub2api.example.com",
                inventory_loader=lambda _db: accounts,
                usage_runner=usage_runner,
                test_runner=lambda *_args, **_kwargs: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_args, **_kwargs: {"success": True, "method": "recover-state"},
                account_reader=lambda _db, account_id: next(
                    (dict(item) for item in accounts if int(item["id"]) == int(account_id)),
                    None,
                ),
            )

            monitor.run_once(NOW)
            monitor.run_once(NOW + timedelta(seconds=2))

            self.assertEqual(called, [1, 2, 3, 4])

    def test_recovery_test_uses_luna_and_creates_success_event(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = summary(five_used=100, seven_used=20, five_reset=NOW)
            refreshed = summary(five_used=0, seven_used=20)
            monitor, calls = self.make_monitor(root, old, refreshed)

            events = monitor.run_once(NOW)

            self.assertEqual(calls["models"], ["gpt-5.6-luna"])
            self.assertEqual(events[0]["status"], "recovered")

    def test_test_failure_is_cached_and_does_not_retest_for_delivery_retry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = summary(five_used=100, seven_used=20, five_reset=NOW)
            refreshed = summary(five_used=0, seven_used=20)
            monitor, calls = self.make_monitor(root, old, refreshed, test_success=False)

            first = monitor.run_once(NOW)
            second = monitor.run_once(NOW + timedelta(seconds=2))

            self.assertEqual(calls["test"], 1)
            self.assertEqual(first[0]["status"], "test_failed")
            self.assertEqual(second, first)

    def test_http_401_and_402_create_auth_events(self) -> None:
        for code in ("http_401", "http_402"):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                old = summary(seven_used=100, seven_reset=NOW)
                monitor, _calls = self.make_monitor(
                    root,
                    old,
                    old,
                    usage_error=(code, "access rejected"),
                )

                events = monitor.run_once(NOW)

                self.assertEqual(events[0]["status"], "auth_failed")
                self.assertEqual(events[0]["error_code"], code)

    def test_early_seven_day_reset_is_detected_before_reset_time(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old_reset = NOW + timedelta(days=4)
            old = summary(seven_used=100, seven_reset=old_reset)
            refreshed = summary(five_used=10, seven_used=10, seven_reset=NOW + timedelta(days=7))
            monitor, calls = self.make_monitor(root, old, refreshed)
            monitor.store.update_scheduler(
                {1: {"last_7d_probe_at": (NOW - timedelta(hours=1)).isoformat()}}
            )

            events = monitor.run_once(NOW)

            self.assertEqual(calls["test"], 1)
            self.assertTrue(events[0]["early_reset_detected"])
            self.assertEqual(events[0]["old_reset_at"], old_reset.isoformat())

    def test_successful_test_self_heals_without_recover_state_call(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(Path(directory), old, refreshed)

            events = monitor.run_once(NOW)

            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 0)
            self.assertEqual(events[0]["status"], "recovered")

    def test_recover_state_runs_after_test_when_block_remains(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(
                Path(directory), old, refreshed, test_clears_block=False
            )

            events = monitor.run_once(NOW)

            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 1)
            self.assertEqual(events[0]["status"], "recovered")

    def test_threshold_block_recovers_at_due_time_below_custom_threshold(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=95, five_reset=NOW)
            refreshed = summary(five_used=10, five_reset=NOW + timedelta(hours=5))
            monitor, calls = self.make_monitor(Path(directory), old, refreshed)
            monitor.db.rows[0].update(
                {
                    "rate_limited_at": None,
                    "rate_limit_reset_at": None,
                    "temp_unschedulable_until": NOW.isoformat(),
                    "temp_unschedulable_reason": json.dumps(
                        {
                            "source": "account_scheduling_threshold",
                            "platform": "openai",
                            "window": "5h",
                            "threshold_percent": 90,
                            "used_percent": 95,
                            "until_unix": int(NOW.timestamp()),
                            "error_message": "threshold reached",
                        }
                    ),
                }
            )

            events = monitor.run_once(NOW)

            self.assertEqual(calls["usage"], 1)
            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 1)
            self.assertEqual(events[0]["status"], "recovered")

    def test_test_may_clear_one_old_block_before_recover_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=10)
            monitor, calls = self.make_monitor(Path(directory), old, refreshed)
            monitor.db.rows[0].update(
                {
                    "temp_unschedulable_until": NOW.isoformat(),
                    "temp_unschedulable_reason": json.dumps(
                        {
                            "source": "account_scheduling_threshold",
                            "platform": "openai",
                            "window": "5h",
                            "threshold_percent": 90,
                            "until_unix": int(NOW.timestamp()),
                            "error_message": "threshold reached",
                        }
                    ),
                }
            )

            events = monitor.run_once(NOW)

            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 1)
            self.assertEqual(events[0]["status"], "recovered")

    def test_threshold_block_still_above_threshold_rechecks_after_60_seconds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            blocked = summary(five_used=95, five_reset=NOW + timedelta(hours=5))
            monitor, calls = self.make_monitor(Path(directory), blocked, blocked)
            monitor.db.rows[0].update(
                {
                    "rate_limited_at": None,
                    "rate_limit_reset_at": None,
                    "temp_unschedulable_until": NOW.isoformat(),
                    "temp_unschedulable_reason": json.dumps(
                        {
                            "source": "account_scheduling_threshold",
                            "platform": "openai",
                            "window": "5h",
                            "threshold_percent": 90,
                            "until_unix": int(NOW.timestamp()),
                            "error_message": "threshold reached",
                        }
                    ),
                }
            )

            monitor.run_once(NOW)
            monitor.run_once(NOW + timedelta(seconds=59))
            monitor.run_once(NOW + timedelta(seconds=60))

            self.assertEqual(calls["usage"], 2)
            self.assertEqual(calls["test"], 0)
            intent = monitor.store.scheduler()[1]["recovery_intent"]
            self.assertEqual(intent["status"], "waiting_quota")
            self.assertEqual(intent["next_retry_at"], (NOW + timedelta(seconds=120)).isoformat())

    def test_recovery_failure_keeps_intent_for_backoff_retry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(
                Path(directory),
                old,
                refreshed,
                test_clears_block=False,
                recovery_success=False,
            )

            events = monitor.run_once(NOW)
            intent = monitor.store.scheduler()[1]["recovery_intent"]

            self.assertEqual(calls["recovery"], 1)
            self.assertEqual(events[0]["status"], "recovery_failed")
            self.assertEqual(intent["status"], "retry")
            self.assertEqual(
                intent["next_retry_at"], (NOW + timedelta(seconds=60)).isoformat()
            )

    def test_test_failure_retries_after_60_seconds_not_before(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(
                Path(directory), old, refreshed, test_success=False
            )

            monitor.run_once(NOW)
            monitor.run_once(NOW + timedelta(seconds=59))
            monitor.run_once(NOW + timedelta(seconds=60))

            self.assertEqual(calls["test"], 2)
            intent = monitor.store.scheduler()[1]["recovery_intent"]
            self.assertEqual(intent["attempt_count"], 2)
            self.assertEqual(
                intent["next_retry_at"], (NOW + timedelta(seconds=360)).isoformat()
            )

    def test_concurrency_signature_change_stops_recovery_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)

            def mutate(row: dict[str, object]) -> None:
                row["concurrency"] = 2

            monitor, calls = self.make_monitor(
                Path(directory),
                old,
                refreshed,
                test_clears_block=False,
                mutate_after_test=mutate,
            )

            events = monitor.run_once(NOW)

            self.assertEqual(calls["recovery"], 0)
            self.assertEqual(events[0]["status"], "recovery_failed")
            self.assertEqual(events[0]["error_code"], "recovery_state_changed")

    def test_night_force_refresh_defers_until_0500_then_recovers_once(self) -> None:
        night = datetime(2026, 1, 10, 16, 0, tzinfo=timezone.utc)
        five_reset = night
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=five_reset)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(Path(directory), old, refreshed)

            night_report = monitor.force_refresh(now=night)
            intent = monitor.store.scheduler()[1]["recovery_intent"]

            self.assertEqual(calls["test"], 0)
            self.assertEqual(night_report["night_deferred_count"], 1)
            self.assertEqual(intent["status"], "deferred")
            self.assertEqual(
                intent["deferred_until"],
                datetime(2026, 1, 10, 21, 0, tzinfo=timezone.utc).isoformat(),
            )

            morning = datetime(2026, 1, 10, 21, 0, tzinfo=timezone.utc)
            morning_report = monitor.force_refresh(now=morning)
            monitor.force_refresh(now=morning + timedelta(minutes=1))

            self.assertEqual(calls["test"], 1)
            self.assertEqual(morning_report["recovered_count"], 1)
            self.assertEqual(
                monitor.store.scheduler()[1]["recovery_intent"]["status"], "recovered"
            )

    def test_night_cooldown_switch_off_allows_recovery(self) -> None:
        night = datetime(2026, 1, 10, 20, 59, tzinfo=timezone.utc)
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.make_monitor(
                Path(directory),
                summary(five_used=100, five_reset=night),
                summary(five_used=0),
            )
            monitor.settings.telegram_oauth_night_recovery_cooldown_enabled = False

            report = monitor.force_refresh(now=night)

            self.assertEqual(calls["test"], 1)
            self.assertEqual(report["night_deferred_count"], 0)
            self.assertEqual(report["recovered_count"], 1)

    def test_force_refresh_ignores_batch_cap_and_queries_all_accounts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            state_path.write_text(
                json.dumps(
                    {
                        "version": 3,
                        "settings": {"sub2api_admin_token": "admin-key"},
                        "oauth_results": {},
                        "scheduler": {},
                        "pending_events": {},
                    }
                ),
                encoding="utf-8",
            )
            rows = [account(value) for value in range(1, 4)]
            called: list[int] = []

            def usage_runner(account_id: int, *_args: object, **_kwargs: object) -> dict[str, object]:
                called.append(account_id)
                payload = result(summary(), NOW)
                payload["account_id"] = account_id
                return payload

            monitor_settings = settings(state_path)
            monitor_settings.telegram_oauth_early_probe_batch_size = 1
            monitor = OAuthMonitor(
                monitor_settings,
                FakeDb(rows),
                base_url_provider=lambda: "https://sub2api.example.com",
                inventory_loader=lambda _db: rows,
                usage_runner=usage_runner,
                test_runner=lambda *_args, **_kwargs: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_args, **_kwargs: {"success": True, "method": "recover-state"},
                account_reader=lambda _db, account_id: next(
                    (dict(item) for item in rows if int(item["id"]) == int(account_id)),
                    None,
                ),
            )

            report = monitor.force_refresh(now=NOW)

            self.assertEqual(sorted(called), [1, 2, 3])
            self.assertEqual(report["queried_count"], 3)
            self.assertEqual(report["success_count"], 3)
            self.assertEqual(
                {value["queried_at"] for value in monitor.store.results().values()},
                {NOW.isoformat()},
            )

    def test_concurrent_force_refresh_calls_share_one_cycle(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            state_path.write_text(
                json.dumps(
                    {
                        "version": 3,
                        "settings": {"sub2api_admin_token": "admin-key"},
                        "oauth_results": {},
                        "scheduler": {},
                        "pending_events": {},
                    }
                ),
                encoding="utf-8",
            )
            entered = threading.Event()
            release = threading.Event()
            second_waiting = threading.Event()
            calls = 0

            def usage_runner(account_id: int, *_args: object, **_kwargs: object) -> dict[str, object]:
                del account_id
                nonlocal calls
                calls += 1
                entered.set()
                if not release.wait(2):
                    raise TimeoutError("usage runner was not released")
                return result(summary(), NOW)

            row = account()
            monitor = OAuthMonitor(
                settings(state_path),
                FakeDb([row]),
                base_url_provider=lambda: "https://sub2api.example.com",
                inventory_loader=lambda _db: [row],
                usage_runner=usage_runner,
                test_runner=lambda *_args, **_kwargs: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_args, **_kwargs: {"success": True, "method": "recover-state"},
                account_reader=lambda _db, account_id: dict(row) if int(account_id) == int(row["id"]) else None,
            )
            original_wait = monitor._force_condition.wait

            def waiting_wait(*args: object, **kwargs: object) -> bool:
                second_waiting.set()
                return bool(original_wait(*args, **kwargs))

            monitor._force_condition.wait = waiting_wait  # type: ignore[method-assign]
            reports: list[dict[str, object]] = []
            errors: list[BaseException] = []

            def run_refresh() -> None:
                try:
                    reports.append(monitor.force_refresh(now=NOW))
                except BaseException as exc:
                    errors.append(exc)

            threads = [threading.Thread(target=run_refresh) for _ in range(2)]

            def cleanup() -> None:
                release.set()
                for thread in threads:
                    thread.join(1)

            self.addCleanup(cleanup)
            threads[0].start()
            self.assertTrue(entered.wait(1))
            threads[1].start()
            self.assertTrue(second_waiting.wait(1))
            release.set()
            for thread in threads:
                thread.join(2)
                self.assertFalse(thread.is_alive())

            self.assertEqual(errors, [])
            self.assertEqual(calls, 1)
            self.assertEqual(len(reports), 2)
            self.assertEqual(sum(bool(item.get("coalesced")) for item in reports), 1)

    def test_force_refresh_timeout_waiting_for_shared_lock_is_explicit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            state_path.write_text(
                json.dumps({"settings": {"sub2api_admin_token": "admin-key"}}),
                encoding="utf-8",
            )
            monitor = OAuthMonitor(
                settings(state_path),
                FakeDb([]),
                base_url_provider=lambda: "https://sub2api.example.com",
                test_runner=lambda *_args, **_kwargs: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_args, **_kwargs: {"success": True, "method": "recover-state"},
            )
            monitor._run_lock.acquire()
            try:
                report = monitor.force_refresh(0.05, now=NOW)
            finally:
                monitor._run_lock.release()

            self.assertFalse(report["success"])
            self.assertTrue(report["timed_out"])

    def test_due_quota_still_depleted_is_rechecked_after_60_seconds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            depleted = summary(five_used=100, five_reset=NOW)
            monitor, calls = self.make_monitor(Path(directory), depleted, depleted)

            monitor.run_once(NOW)
            intent = monitor.store.scheduler()[1]["recovery_intent"]
            monitor.run_once(NOW + timedelta(seconds=59))
            monitor.run_once(NOW + timedelta(seconds=60))

            self.assertEqual(intent["status"], "waiting_quota")
            self.assertEqual(
                intent["next_retry_at"], (NOW + timedelta(seconds=60)).isoformat()
            )
            self.assertEqual(calls["usage"], 2)
            self.assertEqual(calls["test"], 0)

    def test_test_auth_failure_alerts_without_recovery_or_retry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(
                Path(directory), old, refreshed, test_success=False
            )

            def auth_failure(*_args: object, **_kwargs: object) -> dict[str, object]:
                calls["test"] = int(calls["test"]) + 1
                return {"success": False, "error_code": "http_401", "error": "expired"}

            monitor.test_runner = auth_failure
            events = monitor.run_once(NOW)
            intent = monitor.store.scheduler()[1]["recovery_intent"]

            self.assertEqual(calls["recovery"], 0)
            self.assertEqual(events[0]["status"], "auth_failed")
            self.assertEqual(events[0]["stage"], "account_test")
            self.assertEqual(intent["status"], "auth_failed")
            self.assertEqual(intent["next_retry_at"], "")

    def test_account_read_failure_keeps_intent_for_retry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = summary(five_used=100, five_reset=NOW)
            refreshed = summary(five_used=0)
            monitor, calls = self.make_monitor(Path(directory), old, refreshed)
            original_reader = monitor.account_reader
            monitor.account_reader = lambda *_args: (_ for _ in ()).throw(
                RuntimeError("database unavailable")
            )

            events = monitor.run_once(NOW)
            intent = monitor.store.scheduler()[1]["recovery_intent"]

            self.assertEqual(events, [])
            self.assertEqual(calls["test"], 0)
            self.assertEqual(intent["status"], "retry")
            self.assertEqual(intent["last_error_code"], "account_read_failed")
            self.assertEqual(intent["next_retry_at"], (NOW + timedelta(seconds=60)).isoformat())

            monitor.account_reader = original_reader
            monitor.run_once(NOW + timedelta(seconds=60))
            self.assertEqual(calls["test"], 1)

    def test_force_refresh_missing_admin_key_is_explicit_and_does_not_query(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            state_path = root / "usage-query-state.json"
            state_path.write_text("{}", encoding="utf-8")
            calls = 0

            def usage_runner(*_args: object, **_kwargs: object) -> dict[str, object]:
                nonlocal calls
                calls += 1
                return result(summary(), NOW)

            row = account()
            monitor = OAuthMonitor(
                settings(state_path),
                FakeDb([row]),
                base_url_provider=lambda: "https://sub2api.example.com",
                inventory_loader=lambda _db: [row],
                usage_runner=usage_runner,
                test_runner=lambda *_args, **_kwargs: {"success": True, "duration_ms": 1},
                recovery_runner=lambda *_args, **_kwargs: {"success": True, "method": "recover-state"},
                account_reader=lambda _db, account_id: dict(row) if int(account_id) == int(row["id"]) else None,
            )

            report = monitor.force_refresh(now=NOW)

            self.assertFalse(report["success"])
            self.assertEqual(report["error_code"], "missing_sub2api_admin_credentials")
            self.assertEqual(calls, 0)


if __name__ == "__main__":
    unittest.main()
