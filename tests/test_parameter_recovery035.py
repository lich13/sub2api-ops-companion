from __future__ import annotations

import asyncio
import copy
import unittest
from datetime import timedelta
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from fastapi import HTTPException

from app.account_settings import AccountSettings
from app.operation_versions import versions
from tests.test_auto_reset import ACCOUNT_ID, AutoResetFixture


class _SettingsClient:
    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args):
        return None


class ParameterRecovery035Tests(AutoResetFixture):
    """Exercise scalar settings through the real apply and auto-reset step paths."""

    def setUp(self):
        super().setUp()
        self._bind_settings_service()

    def _bind_settings_service(self):
        self.monitor.auto_reset = self.controller
        self.row.update(priority=1, load_factor=None, proxy_id=11)
        self.row["model_mapping"] = {"fixture-model": "fixture-model"}
        self.row["credentials"]["model_mapping"] = {"fixture-model": "fixture-model"}
        self.settings_requests: list[tuple[str, str, dict]] = []
        self.settings_error: HTTPException | None = None

        async def account(account_id: int) -> dict:
            if account_id != ACCOUNT_ID:
                raise HTTPException(404, "fixture account missing")
            return copy.deepcopy(self.row)

        async def request(_client, method: str, path: str, **kwargs):
            self.settings_requests.append((method, path, copy.deepcopy(kwargs)))
            if self.settings_error is not None:
                raise self.settings_error
            if method != "PUT" or path != f"accounts/{ACCOUNT_ID}":
                raise AssertionError(f"unexpected settings request: {method} {path}")
            update = dict(kwargs["json"])
            if update.get("load_factor") == 0:
                update["load_factor"] = None
            self.row.update(update)
            self.touch()
            return {"code": 0}

        self.settings_service = SimpleNamespace(
            r=SimpleNamespace(
                db=self.db,
                settings=self.settings,
                oauth_monitor=self.monitor,
            ),
            actions=SimpleNamespace(
                account=AsyncMock(side_effect=account),
                client=lambda _key, _timeout=10: _SettingsClient(),
                json_request=AsyncMock(side_effect=request),
            ),
            invalidate=Mock(),
        )
        self.account_settings = AccountSettings(self.settings_service)

    def _reset_parameter_case(self):
        self.fresh_case()
        self._bind_settings_service()

    def _seed_retry_hold(self):
        self.test_results.append({"success": False, "error_code": "http_502"})
        self.run_once()
        self.assertEqual(self.calls, ["schedule:False", "reset", "test"])
        task = self.state()
        self.assertEqual(task["stage"], "retry")
        self.assertTrue(task["owns_pause"])
        self.assertFalse(self.row["schedulable"])
        return copy.deepcopy(task)

    def _apply(self, field: str, value: int | None):
        return asyncio.run(self.account_settings.apply(
            ACCOUNT_ID,
            field,
            {field: value},
            versions(self.row)[field],
            "fixture-admin",
        ))

    def _restart_controller(self):
        self.make_controller()
        self.monitor.auto_reset = self.controller
        self.settings_service.r.oauth_monitor = self.monitor
        self.account_settings = AccountSettings(self.settings_service)

    def test_apply_priority_concurrency_and_load_factor_keep_hold_and_retry_verification(self):
        for field, target in (("priority", 7), ("concurrency", 4), ("load_factor", 9)):
            with self.subTest(field=field):
                self._reset_parameter_case()
                self._seed_retry_hold()
                result = self._apply(field, target)

                self.assertTrue(result["verified"])
                self.assertEqual(len(self.settings_requests), 1)
                self.assertEqual(self.settings_requests[0][0:2], ("PUT", f"accounts/{ACCOUNT_ID}"))
                task = self.state()
                self.assertTrue(task["owns_pause"])
                self.assertIsNone(task.get("parameter_edit"))
                self.assertEqual(task["owned_version"], self.row["updated_at"])

                self.now += timedelta(minutes=1)
                self.step()
                self.assertEqual(self.state()["stage"], "recovered")
                self.assertFalse(self.state()["owns_pause"])
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.calls.count("test"), 2)

    def test_intent_save_failure_stops_before_upstream_write(self):
        self._seed_retry_hold()
        failed = []
        original_write = self.store._write

        def fail_before_intent(data):
            task = data["scheduler"][str(ACCOUNT_ID)].get("auto_reset_credit") or {}
            if task.get("parameter_edit") and not failed:
                failed.append(True)
                raise OSError("fixture intent write failed")
            return original_write(data)

        with patch.object(self.store, "_write", side_effect=fail_before_intent):
            with self.assertRaises(OSError):
                self._apply("concurrency", 4)

        self.assertEqual(failed, [True])
        self.assertEqual(self.settings_requests, [])
        task = self.state()
        self.assertTrue(task["owns_pause"])
        self.assertIsNone(task.get("parameter_edit"))

    def test_written_value_is_reconciled_after_restart_without_second_consumption(self):
        self._seed_retry_hold()
        self.controller.reconcile_parameter_edit = lambda _account_id: None

        self._apply("concurrency", 4)
        pending = self.state()
        self.assertEqual(pending["parameter_edit"]["target"], 4)
        self.assertEqual(len(self.settings_requests), 1)
        owned_before = pending["owned_version"]
        self.assertNotEqual(owned_before, self.row["updated_at"])

        self._restart_controller()
        self.step()

        task = self.state()
        self.assertTrue(task["owns_pause"])
        self.assertIsNone(task.get("parameter_edit"))
        self.assertEqual(task["owned_version"], self.row["updated_at"])
        self.assertNotEqual(task["owned_version"], owned_before)
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

    def test_reconcile_save_failure_is_recovered_from_actual_value_after_restart(self):
        self._seed_retry_hold()
        failed = []
        original_write = self.store._write

        def fail_after_upstream(data):
            task = data["scheduler"][str(ACCOUNT_ID)].get("auto_reset_credit") or {}
            if "parameter_edit" in task and task["parameter_edit"] is None and not failed:
                failed.append(True)
                raise OSError("fixture reconcile write failed")
            return original_write(data)

        with patch.object(self.store, "_write", side_effect=fail_after_upstream):
            with self.assertRaises(OSError):
                self._apply("load_factor", 9)

        self.assertEqual(failed, [True])
        self.assertEqual(len(self.settings_requests), 1)
        self.assertEqual(self.row["load_factor"], 9)
        self.assertIn("parameter_edit", self.state())

        self._restart_controller()
        self.step()
        task = self.state()
        self.assertTrue(task["owns_pause"])
        self.assertIsNone(task.get("parameter_edit"))
        self.assertEqual(task["owned_version"], self.row["updated_at"])
        self.assertEqual(self.calls.count("reset"), 1)
        self.assertEqual(self.calls.count("test"), 1)

    def test_rejected_or_undispatched_write_clears_intent_and_keeps_owned_version(self):
        for status_code in (400, 502):
            with self.subTest(status_code=status_code):
                self._reset_parameter_case()
                original = self._seed_retry_hold()
                self.settings_error = HTTPException(status_code, "fixture upstream rejection")
                with self.assertRaises(HTTPException):
                    self._apply("concurrency", 4)

                self.assertEqual(self.settings_requests.__len__(), 1)
                self.assertIn("parameter_edit", self.state())
                self._restart_controller()
                self.step()
                task = self.state()
                self.assertTrue(task["owns_pause"])
                self.assertIsNone(task.get("parameter_edit"))
                self.assertEqual(task["owned_version"], original["owned_version"])
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.calls.count("test"), 1)

    def test_concurrent_generation_credentials_mapping_proxy_and_limit_do_not_resume_hold(self):
        changes = ("generation", "credentials", "mapping", "proxy", "limit")
        for change in changes:
            with self.subTest(change=change):
                self._reset_parameter_case()
                self._seed_retry_hold()
                self.controller.reconcile_parameter_edit = lambda _account_id: None
                self._apply("concurrency", 4)
                pending = self.state()
                owned_before = pending["owned_version"]

                if change == "generation":
                    self.store.manual_control(self.row, True, self.now + timedelta(seconds=1))
                elif change == "credentials":
                    self.row["credentials"]["access_token"] = "fixture-access-v2"
                    self.touch()
                elif change == "mapping":
                    self.row["credentials"]["model_mapping"] = {"fixture-new": "fixture-new"}
                    self.touch()
                elif change == "proxy":
                    self.row["proxy_id"] = 99
                    self.touch()
                else:
                    self.row["rate_limited_at"] = self.now.isoformat()
                    self.row["rate_limit_reset_at"] = (self.now + timedelta(hours=2)).isoformat()
                    self.touch()

                self._restart_controller()
                self.step()
                task = self.state()
                self.assertNotEqual(task.get("owned_version"), self.row["updated_at"])
                self.assertEqual(task.get("owned_version"), owned_before)
                self.assertEqual(self.calls.count("reset"), 1)
                self.assertEqual(self.calls.count("test"), 1)
                if change == "generation":
                    self.assertEqual(task["stage"], "manual")
                    self.assertFalse(task["owns_pause"])
                else:
                    self.assertEqual(task["stage"], "blocked")
                    self.assertFalse(task["owns_pause"])


if __name__ == "__main__":
    unittest.main()
