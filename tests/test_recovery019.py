from __future__ import annotations

import copy
import tempfile
import unittest
from collections import deque
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace

from app.auto_reset import quota_result
from app.oauth_monitor import OAuthMonitor, recovery_quota_ready
from tests.test_auto_reset import AutoResetFixture


NOW = datetime(2026, 10, 1, 8, 0, tzinfo=timezone.utc)


class MonitorDb:
    def __init__(self, row: dict[str, object]) -> None:
        self.row = row

    def fetch_all(self, *_args: object, **_kwargs: object) -> list[dict[str, object]]:
        return []

    def fetch_one(self, _sql: str, params: dict[str, object] | None = None) -> dict[str, object] | None:
        if int((params or {}).get("id") or 0) == int(self.row["id"]):
            return copy.deepcopy(self.row)
        return None


def passive_account(now: datetime = NOW, *, schedulable: bool = True) -> dict[str, object]:
    observed = now - timedelta(seconds=30)
    return {
        "id": 1,
        "name": "recovery-019",
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": schedulable,
        "concurrency": 1,
        "updated_at": (now - timedelta(minutes=2)).isoformat(),
        "credentials": {"plan_type": "plus", "access_token": "recovery-019-token"},
        "extra": {
            "codex_usage_updated_at": observed.isoformat(),
            "codex_5h_used_percent": 10,
            "codex_5h_reset_at": (now + timedelta(hours=4)).isoformat(),
            "codex_7d_used_percent": 20,
            "codex_7d_reset_at": (now + timedelta(days=6)).isoformat(),
        },
        "rate_limited_at": (now - timedelta(minutes=5)).isoformat(),
        "rate_limit_reset_at": (now - timedelta(seconds=61)).isoformat(),
        "overload_until": None,
        "temp_unschedulable_until": None,
        "temp_unschedulable_reason": "",
        "error_message": "",
    }


def monitor_settings(path: Path) -> SimpleNamespace:
    return SimpleNamespace(
        usage_query_state_path=str(path),
        audit_path=str(path.with_name("audit.jsonl")),
        oauth_recovery_monitor_enabled=True,
        oauth_daily_test_enabled=False,
        oauth_auto_reset_credit_enabled=False,
        oauth_usage_refresh_concurrency=1,
        oauth_recovery_test_concurrency=1,
        oauth_early_probe_batch_size=8,
        oauth_recovery_test_model_id="fixture-recovery-model",
    )


def recovery_intent(*, status: str = "ready", now: datetime = NOW) -> dict[str, object]:
    return {
        "fingerprint": "codex_5h@2026-09-30T08:00:00+00:00|codex_7d@2026-09-30T08:00:00+00:00",
        "due_at": (now - timedelta(minutes=2)).isoformat(),
        "source": "server_exact",
        "window_keys": ["codex_5h", "codex_7d"],
        "status": status,
        "deferred_until": "",
        "next_retry_at": "",
        "attempt_count": 0,
        "block_signature": "",
        "confirmed_at": (now - timedelta(minutes=2)).isoformat(),
        "tested_at": "",
        "recovered_at": (now - timedelta(minutes=10)).isoformat() if status == "recovered" else "",
        "last_error": "",
        "last_error_code": "",
    }


class Recovery019MonitorFixture(unittest.TestCase):
    def test_passive_quota_crossing_reset_point_is_reusable(self) -> None:
        row = passive_account()
        result = {
            "success": True,
            "queried_at": (NOW - timedelta(seconds=30)).isoformat(),
            "source": "passive",
            "oauth_quota": {"plan_type": "plus", "ui_windows": [
                {"key": "codex_5h", "used_percent": 10, "reset_at": (NOW - timedelta(minutes=2)).isoformat()},
                {"key": "codex_7d", "used_percent": 20, "reset_at": (NOW - timedelta(minutes=2)).isoformat()},
            ]},
        }
        metadata = {"recovery_intent": {"due_at": (NOW - timedelta(minutes=1)).isoformat()}}
        self.assertTrue(recovery_quota_ready(row, result, metadata, NOW))

    def make_monitor(
        self,
        root: Path,
        row: dict[str, object],
        *,
        test_results: list[dict[str, object]] | None = None,
        test_hook=None,
        recovery_hook=None,
    ) -> tuple[OAuthMonitor, dict[str, int]]:
        db = MonitorDb(row)
        calls = {"usage": 0, "test": 0, "recovery": 0}
        queued_tests = deque(test_results or [{"success": True}])

        def usage_runner(_account_id: int, *_args: object, **kwargs: object) -> dict[str, object]:
            calls["usage"] += 1
            current = kwargs.get("now") or NOW
            current = current if isinstance(current, datetime) else NOW
            return {
                "account_id": 1,
                "template_type": "oauth",
                "success": True,
                "queried_at": current.isoformat(),
                "oauth_quota": {
                    "plan_type": "plus",
                    "ui_windows": [
                        {"key": "codex_5h", "used_percent": 10, "reset_at": (current + timedelta(hours=4)).isoformat()},
                        {"key": "codex_7d", "used_percent": 20, "reset_at": (current + timedelta(days=6)).isoformat()},
                    ],
                },
                "source": "fixture-active",
            }

        def test_runner(_account_id: int, _model_id: str, **_kwargs: object) -> dict[str, object]:
            calls["test"] += 1
            if test_hook:
                test_hook(row)
            return copy.deepcopy(queued_tests.popleft() if queued_tests else {"success": True})

        def recovery_runner(_account_id: int, **_kwargs: object) -> dict[str, object]:
            calls["recovery"] += 1
            if recovery_hook:
                recovery_hook(row)
            else:
                for field in ("rate_limited_at", "rate_limit_reset_at", "overload_until", "temp_unschedulable_until"):
                    row[field] = None
                row["temp_unschedulable_reason"] = ""
            return {"success": True}

        monitor = OAuthMonitor(
            monitor_settings(root / "usage-query-state.json"),
            db,
            base_url_provider=lambda: "https://sub2api.example.com",
            inventory_loader=lambda _db: [copy.deepcopy(row)],
            usage_runner=usage_runner,
            test_runner=test_runner,
            recovery_runner=recovery_runner,
            account_reader=lambda _db, _account_id: copy.deepcopy(row),
            clock=lambda: NOW,
        )
        monitor.store.save_admin_token("fixture-admin-key")
        return monitor, calls

    def seed_intent(self, monitor: OAuthMonitor, intent: dict[str, object], *, quota_query: dict[str, object] | None = None) -> None:
        update: dict[str, object] = {"recovery_intent": intent}
        if quota_query is not None:
            update["quota_query"] = quota_query
        monitor.store.update_scheduler({1: update})

    def test_new_rate_limit_after_recovered_intent_retests_from_passive_quota(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row = passive_account()
            monitor, calls = self.make_monitor(root, row)
            self.seed_intent(monitor, recovery_intent(status="recovered"))

            events = monitor.run_once(now=NOW)

            self.assertEqual(calls["usage"], 0)
            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 1)
            self.assertEqual(events[0]["status"], "recovered")

    def test_failed_test_retries_after_one_minute_without_quota_cooldown_or_budget(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row = passive_account()
            monitor, calls = self.make_monitor(
                root,
                row,
                test_results=[{"success": False, "error_code": "http_502"}, {"success": True}],
            )
            self.seed_intent(monitor, recovery_intent())

            monitor.run_once(now=NOW)
            self.assertEqual(calls["usage"], 0)
            self.assertEqual(calls["test"], 1)
            self.assertEqual(calls["recovery"], 0)
            self.assertEqual(monitor.store.scheduler()[1]["recovery_intent"]["status"], "retry")

            attempts = [(NOW - timedelta(hours=hour)).isoformat() for hour in range(6)]
            monitor.store.update_scheduler({1: {"quota_query": {
                "last_query_at": NOW.isoformat(),
                "last_source": "automatic",
                "automatic_attempts": attempts,
                "failure_count": 0,
                "retry_at": "",
            }}})
            monitor.run_once(now=NOW + timedelta(minutes=1))

            self.assertEqual(calls["usage"], 0)
            self.assertEqual(calls["test"], 2)
            self.assertEqual(calls["recovery"], 1)
            self.assertEqual(monitor.store.scheduler()[1]["recovery_intent"]["status"], "recovered")

    def test_manual_unschedulable_account_is_not_tested_or_recovered(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            row = passive_account(schedulable=False)
            monitor, calls = self.make_monitor(root, row)
            self.seed_intent(monitor, recovery_intent())

            self.assertEqual(monitor.run_once(now=NOW), [])
            self.assertEqual(calls, {"usage": 0, "test": 0, "recovery": 0})

    def test_new_block_or_manual_change_during_test_is_never_cleared(self) -> None:
        for change in ("new_block", "manual"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                row = passive_account()

                def mutate(account: dict[str, object]) -> None:
                    if change == "new_block":
                        account["rate_limited_at"] = NOW.isoformat()
                        account["rate_limit_reset_at"] = (NOW + timedelta(hours=2)).isoformat()
                    else:
                        account["schedulable"] = False
                        account["temp_unschedulable_reason"] = "manual pause"

                monitor, calls = self.make_monitor(root, row, test_hook=mutate)
                self.seed_intent(monitor, recovery_intent())
                monitor.run_once(now=NOW)

                self.assertEqual(calls["usage"], 0)
                self.assertEqual(calls["test"], 1)
                self.assertEqual(calls["recovery"], 0)
                if change == "new_block":
                    self.assertEqual(row["rate_limit_reset_at"], (NOW + timedelta(hours=2)).isoformat())
                else:
                    self.assertFalse(row["schedulable"])


class Recovery019AutoResetTests(AutoResetFixture):
    def test_free_primary_thirty_day_reset_is_preserved_and_available(self) -> None:
        reset_at = NOW + timedelta(days=30)
        row = copy.deepcopy(self.row)
        row["credentials"] = {"plan_type": "free", "access_token": "fixture-free-token"}
        result = quota_result({
            "fetched_at": NOW.isoformat(),
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 2_592_000,
                    "used_percent": 20,
                    "reset_at": int(reset_at.timestamp()),
                    "reset_after_seconds": 2_592_000,
                },
            },
        }, row, NOW)

        self.assertTrue(result["success"])
        self.assertEqual(result["oauth_quota"]["plan_type"], "free")
        windows = result["oauth_quota"]["ui_windows"]
        self.assertEqual([window["key"] for window in windows], ["codex_7d"])
        self.assertEqual(windows[0]["reset_at"], reset_at.isoformat())
        self.assertLess(windows[0]["used_percent"], 100)

    def test_confirmed_no_card_is_not_queried_again_on_the_next_natural_round(self) -> None:
        self.query_credits = 0
        self.row["extra"]["codex_reset_credit_snapshot"] = {}
        self.run_once()
        self.assertEqual(self.calls, ["query"])
        self.assertEqual(self.state()["error_code"], "no_credit")

        self.now += timedelta(hours=1)
        self.run_once()

        self.assertEqual(self.calls, ["query"])
        self.assertEqual(self.state()["error_code"], "no_credit")


if __name__ == "__main__":
    unittest.main()
