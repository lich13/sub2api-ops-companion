from __future__ import annotations

import copy
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

from fastapi import HTTPException
from pydantic import ValidationError

from app.account_operations import AccountOperations, OperationRequest, busy
from app.account_settings import (
    AccountSettings,
    ConcurrencyRequest,
    LoadFactorRequest,
    ProxyRequest,
    capabilities,
    target_reached,
)
from app.operation_versions import versions


VERSION = "a" * 64


def account_row() -> dict:
    return {
        "id": 7,
        "name": "fixture-account",
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": True,
        "priority": 3,
        "concurrency": 8,
        "load_factor": None,
        "proxy_id": 11,
        "parent_account_id": None,
        "group_ids": [13],
        "updated_at": "2026-10-11T00:00:00+00:00",
        "extra": {"quota_remaining": 4, "fixture_flag": "stable"},
        "credential_version": "fixture-credential-digest",
        "credential_settings_version": "fixture-settings-digest",
        "model_mapping_version": "fixture-mapping-digest",
        "model_mapping": {"fixture-model": "fixture-model"},
        "model_sync_project": False,
        "model_catalog_version": "fixture-catalog-digest",
        "credentials": {"api_key": "fixture-key", "model_mapping": {"fixture-model": "fixture-model"}},
    }


class _Client:
    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args):
        return None


class _SettingsFixture:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.row = account_row()
        self.calls: list[tuple[str, str, dict]] = []
        self.proxy_data = [
            {
                "id": 11,
                "name": "fixture proxy",
                "protocol": "https",
                "status": "active",
                "host": "example.invalid",
                "port": 443,
                "username": "fixture-user",
                "password": "fixture-password",
                "url": "https://fixture-user:fixture-password@example.invalid:443",
            }
        ]
        self.sync_response = {
            "models": ["gpt-6-sol", "unsafe model", "gpt-6-sol", "fixture/model:v1", 17],
            "warnings": {"detail": "fixture metadata is hidden"},
        }
        self.db = SimpleNamespace()
        self.settings = SimpleNamespace(
            audit_path=str(Path(self.tmp.name) / "audit.jsonl"),
            usage_query_state_path=str(Path(self.tmp.name) / "state.json"),
        )
        self.r = SimpleNamespace(db=self.db, settings=self.settings)
        self.actions = SimpleNamespace(
            account=AsyncMock(side_effect=self._account),
            client=lambda _key, _timeout=10: _Client(),
        )
        self.actions.json_request = AsyncMock(side_effect=self._request)
        self.invalidate = Mock()
        self.account_settings = AccountSettings(self)

    async def _account(self, account_id: int) -> dict:
        if account_id != self.row["id"]:
            raise HTTPException(404, "fixture account missing")
        return copy.deepcopy(self.row)

    async def _request(self, _client, method: str, path: str, **kwargs):
        self.calls.append((method, path, copy.deepcopy(kwargs)))
        if path == "proxies/all":
            return copy.deepcopy(self.proxy_data)
        if path.endswith("/models/sync-upstream"):
            return copy.deepcopy(self.sync_response)
        if method == "POST" and path == "accounts/bulk-update":
            self.row["model_mapping"] = copy.deepcopy(kwargs["json"]["credentials"]["model_mapping"])
            self.row["model_mapping_version"] = "fixture-mapping-after"
            return {"code": 0}
        if method == "PUT" and path == f"accounts/{self.row['id']}":
            update = kwargs["json"]
            if "proxy_id" in update:
                self.row["proxy_id"] = None if update["proxy_id"] == 0 else update["proxy_id"]
            else:
                self.row.update(update)
            return {"code": 0}
        raise AssertionError(f"unexpected fixture request: {method} {path}")

    def close(self):
        self.tmp.cleanup()


class SettingsContracts035Tests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.fixture = _SettingsFixture()
        self.addCleanup(self.fixture.close)

    async def test_numeric_requests_are_strict_and_effective_load_factor_is_automatic(self):
        for model, field, valid in (
            (ConcurrencyRequest, "concurrency", (1, 2_147_483_647)),
            (LoadFactorRequest, "load_factor", (1, 10_000)),
            (ProxyRequest, "proxy_id", (1, 9_223_372_036_854_775_807)),
        ):
            with self.subTest(model=model.__name__, field=field):
                for value in valid:
                    self.assertEqual(getattr(model(**{field: value, "expected_version": VERSION}), field), value)
                for value in (valid[0] - 1, valid[1] + 1, True, 1.5, "7"):
                    with self.assertRaises(ValidationError):
                        model(**{field: value, "expected_version": VERSION})
        self.assertIsNone(LoadFactorRequest(load_factor=None, expected_version=VERSION).load_factor)

        async def check(value: dict, expected: int):
            self.fixture.row.update(value)
            self.assertEqual((await self.fixture.account_settings.view(7))["effective_load_factor"], expected)

        async def checks():
            await check({"concurrency": 8, "load_factor": None}, 8)
            await check({"concurrency": 8, "load_factor": 17}, 17)
            await check({"concurrency": None, "load_factor": None}, 1)

        await checks()

    def test_settings_versions_are_field_scoped_and_ignore_passive_observations(self):
        row = account_row()
        baseline = versions(row)
        setting_names = ("priority", "concurrency", "load_factor", "model_config", "proxy", "sync_models")

        for field, value in (("priority", 9), ("concurrency", 12), ("load_factor", 5), ("proxy_id", 19)):
            changed = copy.deepcopy(row)
            changed[field] = value
            result = versions(changed)
            self.assertNotEqual(result["priority" if field == "priority" else field if field in result else "proxy"],
                                baseline["priority" if field == "priority" else field if field in result else "proxy"])
            affected = {"proxy", "sync_models"} if field == "proxy_id" else {field}
            for name in setting_names:
                if name in affected:
                    continue
                self.assertEqual(result[name], baseline[name], f"{field} changed {name}")
            if field == "proxy_id":
                self.assertNotEqual(result["sync_models"], baseline["sync_models"])

        for field, value in (
            ("updated_at", "2026-10-12T00:00:00+00:00"),
            ("quota_plan_type", "fixture-paid"),
            ("model_catalog_version", "fixture-catalog-new"),
        ):
            changed = copy.deepcopy(row)
            changed[field] = value
            self.assertEqual({name: versions(changed)[name] for name in setting_names},
                             {name: baseline[name] for name in setting_names}, field)

        changed = copy.deepcopy(row)
        changed["model_mapping_version"] = "fixture-mapping-new"
        self.assertNotEqual(versions(changed)["model_config"], baseline["model_config"])
        for name in ("priority", "concurrency", "load_factor", "proxy"):
            self.assertEqual(versions(changed)[name], baseline[name])

        changed = copy.deepcopy(row)
        changed["credential_settings_version"] = "fixture-settings-new"
        self.assertNotEqual(versions(changed)["model_config"], baseline["model_config"])
        self.assertNotEqual(versions(changed)["sync_models"], baseline["sync_models"])
        for name in ("priority", "concurrency", "load_factor", "proxy"):
            self.assertEqual(versions(changed)[name], baseline[name])

    def test_real_credential_inheritance_and_passthrough_state_controls_capabilities(self):
        independent = capabilities(account_row())
        self.assertTrue(independent["model_config"]["editable"])
        self.assertTrue(independent["proxy"]["editable"])
        self.assertTrue(independent["sync_models"]["editable"])

        inherited = account_row()
        inherited["parent_account_id"] = 3
        inherited_caps = capabilities(inherited)
        for action in ("model_config", "proxy", "sync_models"):
            self.assertFalse(inherited_caps[action]["editable"])
            self.assertTrue(inherited_caps[action]["reason"])
        self.assertFalse(target_reached({**inherited, "model_mapping": {}}, "model_config", {"whitelist": [], "mappings": []}))

        passthrough = account_row()
        passthrough["extra"] = {"openai_passthrough": True}
        passthrough_caps = capabilities(passthrough)
        self.assertFalse(passthrough_caps["model_config"]["editable"])
        self.assertTrue(passthrough_caps["proxy"]["editable"])
        self.assertIn("透传", passthrough_caps["model_config"]["reason"])

        changed_parent = copy.deepcopy(account_row())
        changed_parent["parent_account_id"] = 3
        self.assertNotEqual(versions(changed_parent)["model_config"], versions(account_row())["model_config"])

    async def test_model_config_writes_only_local_mapping_and_proxy_can_clear_with_zero(self):
        before = copy.deepcopy(self.fixture.row)
        payload = {"whitelist": ["fixture-new-model"], "mappings": []}
        result = await self.fixture.account_settings.apply(
            7, "model_config", payload, versions(self.fixture.row)["model_config"], "fixture-admin"
        )
        self.assertTrue(result["verified"])
        method, path, kwargs = self.fixture.calls[-1]
        self.assertEqual((method, path), ("POST", "accounts/bulk-update"))
        self.assertEqual(kwargs["json"], {"account_ids": [7], "credentials": {"model_mapping": {"fixture-new-model": "fixture-new-model"}}})
        for key in before.keys() - {"model_mapping", "model_mapping_version"}:
            self.assertEqual(self.fixture.row[key], before[key], key)

        self.fixture.calls.clear()
        self.fixture.row["proxy_id"] = 11
        await self.fixture.account_settings.apply(7, "proxy", {"proxy_id": None}, versions(self.fixture.row)["proxy"], "fixture-admin")
        self.assertEqual(self.fixture.calls[-1][2]["json"], {"proxy_id": 0})
        self.assertIsNone(self.fixture.row["proxy_id"])
        self.assertEqual(self.fixture.row["group_ids"], before["group_ids"])
        self.assertEqual(self.fixture.row["schedulable"], before["schedulable"])
        self.assertEqual(self.fixture.row["credentials"], before["credentials"])

    async def test_sync_catalog_posts_only_safe_model_ids_and_never_runs_test_or_quota(self):
        result = await self.fixture.account_settings.apply(
            7, "sync_models", {}, versions(self.fixture.row)["sync_models"], "fixture-admin"
        )
        self.assertEqual(result["models"], ["gpt-6-sol", "fixture/model:v1"])
        self.assertEqual(result["warnings"], ["模型能力信息不完整"])
        self.assertEqual([(method, path) for method, path, _kwargs in self.fixture.calls],
                         [("POST", "accounts/7/models/sync-upstream")])
        self.assertFalse(any("test" in path or "quota" in path for _method, path, _kwargs in self.fixture.calls))

    async def test_proxy_dto_exposes_no_address_or_authentication_fields(self):
        result = await self.fixture.account_settings.proxies("fixture-admin")
        self.assertEqual(result, [{"id": 11, "name": "fixture proxy", "protocol": "https", "status": "active"}])
        serialized = repr(result)
        for secret in ("example.invalid", "fixture-user", "fixture-password", "443", "https://"):
            self.assertNotIn(secret, serialized)


class OperationsContracts035Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addAsyncCleanup(self._cleanup)
        self.row = account_row()
        self.db = SimpleNamespace()
        self.service = SimpleNamespace(
            r=SimpleNamespace(
                db=self.db,
                settings=SimpleNamespace(
                    usage_query_state_path=str(Path(self.tmp.name) / "state.json"),
                    audit_path=str(Path(self.tmp.name) / "audit.jsonl"),
                    ),
                key_fallback_controller=None,
                oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: "fixture-admin"),
            ),
            actions=SimpleNamespace(account=AsyncMock(side_effect=self._account)),
            account_settings=SimpleNamespace(apply=AsyncMock()),
            authenticate=Mock(),
            invalidate=Mock(),
        )
        self.queue = AccountOperations(self.service)

    async def _account(self, account_id: int) -> dict:
        if account_id != self.row["id"]:
            raise HTTPException(404, "fixture account missing")
        return copy.deepcopy(self.row)

    async def _cleanup(self):
        await self.queue.close()
        self.tmp.cleanup()

    async def submit(self, action: str, payload: dict, request_id: str, expected: str | None = None):
        return await self.queue.submit(
            7,
            OperationRequest(
                action=action,
                payload=payload,
                request_id=request_id,
                client_id="fixture-client-0001",
                expected_version=expected or versions(self.row)[action],
            ),
            "fixture-admin",
        )

    async def test_busy_settings_operation_remains_queued_then_runs_once(self):
        self.service.account_settings.apply.side_effect = [busy(), {"verified": True, "concurrency": 12}]
        job = await self.submit("concurrency", {"concurrency": 12}, "fixture-concurrency-0001")
        await self.queue.run(job["id"])
        self.assertEqual(self.queue.get(job["id"])["status"], "queued")
        self.assertEqual(self.service.account_settings.apply.await_count, 1)
        await self.queue.run(job["id"])
        self.assertEqual(self.queue.get(job["id"])["status"], "completed")
        self.assertEqual(self.service.account_settings.apply.await_count, 2)

    async def test_unchanged_settings_operation_is_verified_without_write(self):
        job = await self.submit("concurrency", {"concurrency": self.row["concurrency"]}, "fixture-concurrency-0002")
        await self.queue.run(job["id"])
        result = self.queue.get(job["id"])
        self.assertEqual(result["status"], "completed")
        self.assertTrue(result["result"]["verified"])
        self.service.account_settings.apply.assert_not_awaited()

    async def test_timeout_enters_confirmation_and_does_not_replay_write(self):
        self.service.account_settings.apply.side_effect = HTTPException(504, {"code": "fixture-timeout"})
        job = await self.submit("concurrency", {"concurrency": 12}, "fixture-concurrency-0003")
        await self.queue.run(job["id"])
        self.assertEqual(self.queue.get(job["id"])["status"], "checking")
        await self.queue.run(job["id"])
        self.assertEqual(self.queue.get(job["id"])["status"], "needs_confirmation")
        self.assertEqual(self.service.account_settings.apply.await_count, 1)

    async def test_priority_does_not_cancel_recovery_or_use_card_intents(self):
        priority = await self.submit("priority", {"priority": 9}, "fixture-priority-0001")
        recovery = await self.submit("recover", {}, "fixture-recover-0001", versions(self.row)["recover"])
        use_card = await self.submit(
            "reset_quota", {"action": "reset_quota", "confirmed": True}, "fixture-reset-0001",
            versions(self.row)["reset_quota"],
        )
        self.assertEqual({self.queue.get(item["id"])["status"] for item in (priority, recovery, use_card)}, {"queued"})
        self.assertEqual(self.queue.get(recovery["id"])["action"], "recover")
        self.assertEqual(self.queue.get(use_card["id"])["action"], "reset_quota")


if __name__ == "__main__":
    unittest.main()
