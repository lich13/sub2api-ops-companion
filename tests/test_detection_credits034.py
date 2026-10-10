from __future__ import annotations

import copy
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

from app.capacity_alerts import CapacityAlertStore
from app.model_detection import DEFAULT_MODEL, TARGET, ModelDetection
from app.oauth_monitor import OAuthStateStore
from app.oauth_queries import credential_fingerprint


NOW = datetime(2026, 10, 9, 3, tzinfo=timezone.utc)


class Clock:
    def __init__(self, value: datetime = NOW):
        self.value = value

    def __call__(self) -> datetime:
        return self.value


def oauth_row(*, extra: dict | None = None, account_id: int = 1, **changes) -> dict:
    return {
        "id": account_id,
        "name": "fixture-oauth",
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "deleted_at": None,
        "parent_account_id": None,
        "group_ids": [],
        "credentials": {"plan_type": "plus", "access_token": "fixture-token"},
        "extra": dict(extra or {}),
        **changes,
    }


def key_row(*, extra: dict | None = None, account_id: int = 1, **changes) -> dict:
    return {
        "id": account_id,
        "name": "fixture-key",
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": True,
        "deleted_at": None,
        "parent_account_id": None,
        "credentials": {"api_key": "fixture-key"},
        "extra": dict(extra or {}),
        **changes,
    }


def credits(*, fetched_at: datetime = NOW, has_credits: bool = True,
            unlimited: bool = False, balance: object = 3) -> dict:
    return {
        "codex_credits_snapshot": {
            "credits": {"has_credits": has_credits, "unlimited": unlimited, "balance": balance},
            # Native Sub2API evidence uses Unix seconds rather than an ISO string.
            "fetched_at": fetched_at.timestamp(),
        }
    }


def full_quota(*, queried_at: datetime = NOW) -> dict:
    return {
        "success": True,
        "queried_at": queried_at.isoformat(),
        "oauth_quota": {
            "plan_type": "plus",
            "ui_windows": [
                {"key": "codex_5h", "used_percent": 100, "remaining_percent": 0,
                 "reset_at": (NOW + timedelta(hours=1)).isoformat()},
                {"key": "codex_7d", "used_percent": 100, "remaining_percent": 0,
                 "reset_at": (NOW + timedelta(days=1)).isoformat()},
            ],
        },
    }


class DetectionCredits034Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.clock = Clock()
        self.row = oauth_row(extra={**credits(), "codex_usage_updated_at": (NOW - timedelta(hours=2)).isoformat()})
        self.capacity = SimpleNamespace(store=CapacityAlertStore(root / "capacity.json"))
        self.oauth_store = OAuthStateStore(str(root / "oauth-state.json"))
        self.jobs: dict[str, dict] = {}

        async def start(aid, _payload, automatic=None):
            job = {
                "id": "fixture-job-034",
                "account_id": aid,
                "status": "queued",
                "automatic": bool(automatic),
                "requested_model": DEFAULT_MODEL,
                "triggers": list((automatic or {}).get("triggers") or []),
                "detection_generation": (automatic or {}).get("detection_generation"),
                "detection_mark_version": (automatic or {}).get("detection_mark_version"),
            }
            self.jobs[job["id"]] = job
            return copy.deepcopy(job)

        self.model_tests = SimpleNamespace(
            account=AsyncMock(side_effect=lambda _aid: copy.deepcopy(self.row)),
            latest=Mock(return_value=None),
            get=Mock(side_effect=lambda job_id: copy.deepcopy(self.jobs[job_id])),
            start=AsyncMock(side_effect=start),
            cancel=AsyncMock(),
        )
        self.actions = SimpleNamespace(
            account=AsyncMock(side_effect=lambda _aid: copy.deepcopy(self.row)),
            model_allowed=AsyncMock(return_value=True),
        )
        self.service = SimpleNamespace(
            invalidate=Mock(),
            model_tests=self.model_tests,
            actions=self.actions,
            r=SimpleNamespace(
                db=SimpleNamespace(),
                settings=SimpleNamespace(
                    usage_query_state_path=str(root / "usage.json"),
                    audit_path=str(root / "audit.jsonl"),
                ),
                capacity_alerts=self.capacity,
                oauth_monitor=SimpleNamespace(store=self.oauth_store),
                fingerprint_bank=SimpleNamespace(
                    capture=Mock(return_value=({"models": [{"id": TARGET}]}, "fixture-bank"))
                ),
            ),
        )
        self.detection = ModelDetection(self.service, clock=self.clock)

    def _save_quota(self, result: dict | None = None) -> None:
        if result is not None:
            self.oauth_store.commit(results={1: result})

    def _enable_due(self, *, interval_minutes: int = 15) -> None:
        self.detection.update(
            1, enabled=True, interval_minutes=interval_minutes, model_id=DEFAULT_MODEL,
            status="waiting", reason="", next_at=self.clock().isoformat(),
        )

    async def test_tick_allows_full_display_window_when_fresh_native_credits_are_available(self):
        self._save_quota(full_quota(queried_at=NOW - timedelta(hours=2)))
        self._enable_due()

        await self.detection.tick()

        self.model_tests.start.assert_awaited_once()
        self.assertEqual(self.detection.view(1)["status"], "queued")

    async def test_tick_uses_native_credit_freshness_independently_from_a_fresh_full_window(self):
        # The UI quota is at exactly 100%, while the native balance was still
        # positive 51 minutes ago. The two observations have separate clocks.
        self.row["extra"] = credits(fetched_at=NOW - timedelta(minutes=51), balance=None)
        self._save_quota(full_quota(queried_at=NOW))
        self._enable_due()

        await self.detection.tick()

        self.model_tests.start.assert_awaited_once()

    async def test_tick_allows_full_window_for_unlimited_or_unknown_native_credits(self):
        cases = (
            credits(has_credits=False, unlimited=True, balance=0),
            {},
            credits(fetched_at=NOW - timedelta(hours=1, seconds=1), has_credits=False, balance=0),
        )
        for extra in cases:
            with self.subTest(extra=extra):
                self.row["extra"] = {**extra}
                self.actions.account.reset_mock()
                self.model_tests.start.reset_mock()
                self.jobs.clear()
                with self.detection.store.transaction() as data:
                    data["accounts"] = {}
                    data["consumed"] = {}
                    data.pop("automatic_starts", None)
                self.detection = ModelDetection(self.service, clock=self.clock)
                self._save_quota(full_quota())
                self._enable_due()

                await self.detection.tick()

                self.model_tests.start.assert_awaited_once()
                self.assertEqual(self.detection.view(1)["status"], "queued")

    async def test_tick_allows_full_window_for_invalid_or_contradictory_native_evidence(self):
        cases = (
            credits(has_credits=False, balance=1),
            credits(has_credits=True, balance="NaN"),
            credits(has_credits=True, balance=-1),
            credits(fetched_at=NOW + timedelta(seconds=1), has_credits=False, balance=0),
        )
        for extra in cases:
            with self.subTest(extra=extra):
                self.row["extra"] = extra
                self.model_tests.start.reset_mock()
                self.jobs.clear()
                with self.detection.store.transaction() as data:
                    data["accounts"] = {}
                    data["consumed"] = {}
                    data.pop("automatic_starts", None)
                self.detection = ModelDetection(self.service, clock=self.clock)
                self._save_quota(full_quota())
                self._enable_due()

                await self.detection.tick()

                self.model_tests.start.assert_awaited_once()

    async def test_tick_pauses_only_when_native_credits_are_empty_and_full_window_is_fresh(self):
        self.row["extra"] = credits(has_credits=False, unlimited=False, balance=0)
        self._save_quota(full_quota())
        self._enable_due()

        await self.detection.tick()

        self.model_tests.start.assert_not_awaited()
        state = self.detection.view(1)
        self.assertEqual(state["status"], "paused")
        self.assertEqual(state["reason"], "已确认额度耗尽")

    async def test_tick_uses_native_usage_percentages_without_mixing_freshness_or_decimal_balance(self):
        cases = (
            ({"used": 99.999, "credits": credits(has_credits=False, balance=0)}, "queued"),
            ({"used": 100, "credits": credits(has_credits=False, balance=0)}, "paused"),
            ({"used": 100, "reset_at": NOW - timedelta(seconds=1),
              "credits": credits(has_credits=False, balance=0)}, "queued"),
            ({"used": 100, "credits": credits(has_credits=True, balance="0.01")}, "queued"),
        )
        for case, expected_status in cases:
            with self.subTest(case=case):
                used = case["used"]
                reset_at = case.get("reset_at", NOW + timedelta(hours=1))
                extra = {
                    "codex_usage_updated_at": NOW.isoformat(),
                    "codex_5h_used_percent": used,
                    "codex_7d_used_percent": used,
                    "codex_5h_reset_at": reset_at.isoformat(),
                    "codex_7d_reset_at": (reset_at if reset_at <= NOW else reset_at + timedelta(days=1)).isoformat(),
                    **case["credits"],
                }
                self.row["extra"] = extra
                self.model_tests.start.reset_mock()
                self.jobs.clear()
                with self.detection.store.transaction() as data:
                    data["accounts"] = {}
                    data["consumed"] = {}
                    data.pop("automatic_starts", None)
                self.detection = ModelDetection(self.service, clock=self.clock)
                self._enable_due()

                await self.detection.tick()

                self.assertEqual(self.detection.view(1)["status"], expected_status)
                if expected_status == "queued":
                    self.model_tests.start.assert_awaited_once()
                else:
                    self.model_tests.start.assert_not_awaited()

    def test_reason_preserves_precedence_for_manual_close_rate_limit_and_auth(self):
        available = oauth_row(extra=credits())
        self.assertEqual(self.detection.reason({**available, "schedulable": False}, {}), "调度已关闭")

        limited = {**available, "rate_limit_reset_at": (NOW + timedelta(minutes=5)).isoformat()}
        self.assertEqual(self.detection.reason(limited, {}), "账号仍在上游限流中")

        self.oauth_store.commit(scheduler_updates={1: {
            "quota_query": {"auth_fingerprint": credential_fingerprint(available)},
        }})
        self.assertEqual(self.detection.reason(available, {}), "认证异常，等待凭据更新")

    def test_old_rejection_before_recovery_point_is_ignored_but_new_fresh_rejection_blocks(self):
        row = oauth_row(extra=credits(), rate_limit_reset_at=(NOW - timedelta(seconds=1)).isoformat(),
                        last_error_code="quota_exceeded", last_error_at=(NOW - timedelta(hours=2)).isoformat())
        self.assertEqual(self.detection.reason(row, {}), "")

        self._save_quota({"template_type": "oauth", "success": False,
                          "error_code": "quota_exceeded", "queried_at": NOW.isoformat()})

        self.assertEqual(self.detection.reason(row, {}), "上游已拒绝额度请求")

    def test_key_detection_still_uses_configured_actual_limits(self):
        row = key_row(extra={
            "quota_daily_limit": 100,
            "quota_daily_used": 100,
            "quota_daily_reset_mode": "fixed",
            "quota_daily_reset_at": (NOW + timedelta(hours=1)).isoformat(),
        })
        self.assertEqual(self.detection.reason(row, {}), "已确认额度耗尽")

    async def test_recovered_from_credit_pause_restarts_timer_and_respects_five_minute_gate(self):
        self.row["extra"] = credits(has_credits=False, unlimited=False, balance=0)
        self._save_quota(full_quota())
        self._enable_due(interval_minutes=1)
        await self.detection.tick()
        self.assertEqual(self.detection.view(1)["status"], "paused")

        # A previous automatic dispatch still owns the five-minute admission window.
        with self.detection.store.transaction() as data:
            data.setdefault("automatic_starts", {})["1"] = {
                "job_id": "fixture-previous-job",
                "started_at": (NOW - timedelta(seconds=100)).isoformat(),
            }
        self.row["extra"] = credits(has_credits=True, unlimited=False, balance=3)
        self.detection.update(1, status="paused", reason="已确认额度耗尽", next_at=None)

        await self.detection.tick()

        state = self.detection.view(1)
        self.assertEqual(state["status"], "waiting")
        self.assertEqual(
            datetime.fromisoformat(state["next_at"]),
            NOW + timedelta(seconds=200),
        )
        self.model_tests.start.assert_not_awaited()


if __name__ == "__main__":
    unittest.main()
