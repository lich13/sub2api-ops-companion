from __future__ import annotations

import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace

from app.oauth_monitor import (
    OAuthMonitor,
    OAuthStateStore,
    build_monitor_candidates,
    migrate_legacy_recovery_state,
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
        "schedulable": True,
    }


def settings(path: Path) -> SimpleNamespace:
    return SimpleNamespace(
        usage_query_state_path=str(path),
        audit_path=str(path.with_name("audit.jsonl")),
        telegram_oauth_usage_refresh_enabled=True,
        telegram_oauth_recovery_monitor_enabled=True,
        telegram_oauth_recovery_push_enabled=True,
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


class OAuthStateStoreTests(unittest.TestCase):
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


class OAuthMonitorExecutionTests(unittest.TestCase):
    def make_monitor(
        self,
        root: Path,
        old_summary: dict[str, object],
        refreshed_summary: dict[str, object],
        *,
        usage_error: tuple[str, str] | None = None,
        test_success: bool = True,
    ) -> tuple[OAuthMonitor, dict[str, object]]:
        state_path = root / "usage-query-state.json"
        state_path.write_text(
            json.dumps(
                {
                    "version": 2,
                    "settings": {"sub2api_admin_token": "admin-key"},
                    "oauth_results": {"1": result(old_summary, NOW - timedelta(hours=2))},
                    "scheduler": {},
                    "pending_events": {},
                }
            ),
            encoding="utf-8",
        )
        calls: dict[str, object] = {"usage": 0, "test": 0, "models": []}

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
            if test_success:
                return {"success": True, "model_id": model_id, "duration_ms": 18}
            return {
                "success": False,
                "model_id": model_id,
                "duration_ms": 18,
                "error_code": "http_502",
                "error": "upstream failed",
            }

        monitor = OAuthMonitor(
            settings(state_path),
            FakeDb([account()]),
            base_url_provider=lambda: "https://sub2api.example.com",
            inventory_loader=lambda _db: [account()],
            usage_runner=usage_runner,
            test_runner=test_runner,
        )
        return monitor, calls

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
                test_runner=lambda *_args, **_kwargs: {"success": True},
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


if __name__ == "__main__":
    unittest.main()
