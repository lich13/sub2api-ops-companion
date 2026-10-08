from __future__ import annotations

import asyncio
import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fastapi import HTTPException
from app.account_locks import account_lock
from app.account_operations import AccountOperations
from app.account_templates import AccountTemplates, CustomTemplateRequest, TemplateBatchRequest, TemplatesRequest
from app.operation_versions import digest, versions


ADMIN_FIXTURE = "fixture-admin-key-must-not-persist"
REQUEST_ID = "a" * 32
CLIENT_ID = "b" * 32


class BatchDB:
    def __init__(self, account_count: int = 40):
        self.rows = {
            aid: {
                "id": aid,
                "name": f"fixture-account-{aid}",
                "platform": "openai",
                "type": "oauth",
                "deleted_at": None,
                "parent_account_id": None,
                "passthrough": False,
                "model_mapping": {},
                "model_mapping_version": digest({}),
                "credential_version": f"fixture-credential-version-{aid}",
                "credentials": {"access_token": f"fixture-access-secret-{aid}"},
                "status": "active",
                "schedulable": True,
                "priority": 1,
                "group_ids": [],
                "extra": {"fixture_account_setting": f"preserve-{aid}"},
                "updated_at": "fixture-account-version-1",
            }
            for aid in range(1, account_count + 1)
        }

    def fetch_one(self, _sql, params):
        return copy.deepcopy(self.rows.get(int(params["id"])))

    def fetch_all(self, _sql, _params=None):
        return copy.deepcopy(list(self.rows.values()))


class BatchActions:
    def __init__(self, db: BatchDB):
        self.db = db

    async def account(self, account_id: int):
        row = self.db.rows.get(account_id)
        if row is None:
            raise KeyError(account_id)
        return copy.deepcopy(row)


class AccountTemplateBatch031Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="account-batch-")
        self.addAsyncCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.db = BatchDB()
        self.write_calls: list[tuple[int, dict[str, str], str]] = []
        settings = SimpleNamespace(
            usage_query_state_path=str(root / "oauth-state.json"),
            audit_path=str(root / "audit.jsonl"),
        )
        self.service = SimpleNamespace()
        self.service.r = SimpleNamespace(
            db=self.db,
            settings=settings,
            key_fallback_controller=None,
            oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: ADMIN_FIXTURE),
        )
        self.service.actions = BatchActions(self.db)
        self.service.authenticate = lambda *_args, **_kwargs: None
        self.service.invalidate = lambda: None
        self.templates = AccountTemplates(self.service)
        self.service.account_templates = self.templates
        self.templates.writer = self.write_mapping
        initial = self.templates.view()
        self.template_view = self.templates.save(TemplatesRequest(
            expected_version=initial["version"],
            full={"whitelist": ["fixture-full-model"],
                  "mappings": [{"source": "fixture-input-*", "target": "fixture-full-model"}]},
            degraded={"whitelist": ["fixture-degraded-model"], "mappings": []},
            takeover={"whitelist": ["fixture-takeover-model"], "mappings": []},
        ))
        self.queue = AccountOperations(self.service)

    async def asyncTearDown(self):
        await self.queue.close()

    def write_mapping(self, account_id: int, mapping: dict[str, str], key: str):
        self.write_calls.append((account_id, copy.deepcopy(mapping), key))
        self.db.rows[account_id]["model_mapping"] = copy.deepcopy(mapping)
        self.db.rows[account_id]["model_mapping_version"] = digest(mapping)
        return "ok"

    def batch_request(self, account_ids: list[int], *, request_id: str = REQUEST_ID,
                      client_id: str = CLIENT_ID, template_id: str = "full") -> TemplateBatchRequest:
        view = self.templates.view()
        return TemplateBatchRequest(
            template_id=template_id,
            template_version=view["template_versions"][template_id],
            accounts=[{"account_id": aid,
                       "expected_version": versions(self.db.rows[aid])["account_template"]}
                      for aid in account_ids],
            request_id=request_id,
            client_id=client_id,
        )

    async def final(self, job_id: str):
        for _ in range(150):
            result = self.queue.get(job_id)
            if result["status"] not in {"queued", "running", "checking"}:
                return result
            await asyncio.sleep(0.02)
        self.fail("batch child did not settle")

    async def start_loop(self):
        self.loop = asyncio.create_task(self.queue.loop())

        async def stop():
            self.loop.cancel()
            await asyncio.gather(self.loop, return_exceptions=True)

        self.addAsyncCleanup(stop)

    async def test_batch_commit_is_atomic_idempotent_and_secret_free(self):
        request = self.batch_request([1, 2])
        first, duplicate = await asyncio.gather(
            self.queue.submit_template_batch(request, ADMIN_FIXTURE),
            self.queue.submit_template_batch(request, ADMIN_FIXTURE),
        )

        self.assertEqual(first["batch_id"], duplicate["batch_id"])
        self.assertEqual(len(first["items"]), 2)
        self.assertEqual(len(self.queue.listing(batch_id=first["batch_id"])["items"]), 2)
        persisted = json.loads(self.queue.store.path.read_text())
        self.assertEqual(len(persisted["batches"]), 1)
        self.assertEqual(len(persisted["jobs"]), 2)
        self.assertTrue(all(job["batch_id"] == first["batch_id"] for job in persisted["jobs"].values()))
        stored = self.queue.store.path.read_text()
        self.assertNotIn(ADMIN_FIXTURE, stored)
        self.assertNotIn("fixture-access-secret", stored)
        self.assertNotIn(ADMIN_FIXTURE, Path(self.service.r.settings.audit_path).read_text())

        failed_request = self.batch_request([3], request_id="c" * 32)
        with patch("app.account_operations.write_json", side_effect=OSError("fixture atomic write failure")):
            with self.assertRaisesRegex(OSError, "fixture atomic write failure"):
                await self.queue.submit_template_batch(failed_request, ADMIN_FIXTURE)
        after_failure = self.queue.store.read()
        self.assertEqual(len(after_failure["batches"]), 1)
        self.assertEqual(len(after_failure["jobs"]), 2)

        with self.assertRaises(HTTPException) as duplicate_accounts:
            await self.queue.submit_template_batch(
                self.batch_request([4, 4], request_id="d" * 32), ADMIN_FIXTURE,
            )
        self.assertEqual(duplicate_accounts.exception.status_code, 422)
        self.assertEqual(len(self.queue.store.read()["batches"]), 1)

    async def test_busy_child_does_not_block_other_accounts_and_batch_remains_readable(self):
        await self.start_loop()
        lock = account_lock(self.db, 1)
        self.assertTrue(lock.acquire(blocking=False))
        try:
            submitted = await self.queue.submit_template_batch(
                self.batch_request([1, 2, 3]), ADMIN_FIXTURE,
            )
            second = next(item for item in submitted["items"] if item["account_id"] == 2)
            third = next(item for item in submitted["items"] if item["account_id"] == 3)
            first = next(item for item in submitted["items"] if item["account_id"] == 1)

            self.assertEqual((await self.final(second["id"]))["status"], "completed")
            self.assertEqual((await self.final(third["id"]))["status"], "completed")
            self.assertEqual(self.queue.get(first["id"])["status"], "queued")
            self.assertEqual({aid for aid, _mapping, _key in self.write_calls}, {2, 3})
        finally:
            lock.release()

        self.assertEqual((await self.final(first["id"]))["status"], "completed")
        history = self.queue.listing(batch_id=submitted["batch_id"])
        self.assertEqual({item["account_id"] for item in history["items"]}, {1, 2, 3})
        self.assertEqual({item["status"] for item in history["items"]}, {"completed"})

    async def test_listing_by_batch_returns_full_terminal_history(self):
        submitted = await self.queue.submit_template_batch(
            self.batch_request(list(range(1, 36))), ADMIN_FIXTURE,
        )
        for child in submitted["items"]:
            self.queue.update(child["id"], status="completed", reason="fixture completed")

        default_items = self.queue.listing()["items"]
        full_history = self.queue.listing(batch_id=submitted["batch_id"])["items"]
        self.assertLess(len(default_items), 35)
        self.assertEqual(len(full_history), 35)
        self.assertEqual({item["batch_id"] for item in full_history}, {submitted["batch_id"]})

    async def test_queued_batch_survives_service_restart_without_duplicate_children(self):
        request = self.batch_request([1, 2])
        submitted = await self.queue.submit_template_batch(request, ADMIN_FIXTURE)
        child_ids = {item["id"] for item in submitted["items"]}
        self.queue = AccountOperations(self.service)

        duplicate = await self.queue.submit_template_batch(request, ADMIN_FIXTURE)
        resumed = self.queue.listing(batch_id=submitted["batch_id"])["items"]
        self.assertEqual(duplicate["batch_id"], submitted["batch_id"])
        self.assertEqual({item["id"] for item in resumed}, child_ids)
        self.assertTrue(all(item["status"] == "queued" for item in resumed))

        for child_id in child_ids:
            await self.queue.run(child_id)

        self.assertEqual({item["status"] for item in self.queue.listing(batch_id=submitted["batch_id"])["items"]},
                         {"completed"})
        self.assertEqual([item[0] for item in self.write_calls].count(1), 1)
        self.assertEqual([item[0] for item in self.write_calls].count(2), 1)

    async def test_target_template_change_requires_confirmation_without_account_write(self):
        submitted = await self.queue.submit_template_batch(self.batch_request([1]), ADMIN_FIXTURE)
        child = submitted["items"][0]
        before = copy.deepcopy(self.db.rows[1])
        current = self.templates.view()
        self.templates.update_custom("full", CustomTemplateRequest(
            expected_version=current["version"],
            name=current["template_meta"]["full"]["name"],
            whitelist=["fixture-changed-target"], mappings=[],
        ))

        await self.queue.run(child["id"])

        result = self.queue.get(child["id"])
        self.assertEqual(result["status"], "needs_confirmation")
        self.assertEqual(self.write_calls, [])
        self.assertEqual(self.db.rows[1], before)

    async def test_queued_batch_target_deletion_requires_confirmation_without_account_write(self):
        submitted = await self.queue.submit_template_batch(self.batch_request([1]), ADMIN_FIXTURE)
        child = submitted["items"][0]
        before = copy.deepcopy(self.db.rows[1])
        current = self.templates.view()
        self.templates.delete_custom("full", current["version"])

        await self.queue.run(child["id"])

        self.assertEqual(self.queue.get(child["id"])["status"], "needs_confirmation")
        self.assertEqual(self.write_calls, [])
        self.assertEqual(self.db.rows[1], before)
        self.assertEqual(self.queue.listing(batch_id=submitted["batch_id"])["pending"], 1)

    async def test_dispatched_timeout_checks_frozen_target_after_template_edit_and_deletion(self):
        full_batch = await self.queue.submit_template_batch(
            self.batch_request([1], request_id="c" * 32, template_id="full"), ADMIN_FIXTURE,
        )
        degraded_batch = await self.queue.submit_template_batch(
            self.batch_request([2], request_id="d" * 32, template_id="degraded"), ADMIN_FIXTURE,
        )
        full_child, degraded_child = full_batch["items"][0], degraded_batch["items"][0]

        def accepted_then_timeout(account_id, mapping, key):
            self.write_mapping(account_id, mapping, key)
            raise HTTPException(status_code=504, detail={"code": "fixture_timeout"})

        self.templates.writer = accepted_then_timeout
        await self.queue.run(full_child["id"])
        await self.queue.run(degraded_child["id"])
        self.assertEqual(self.queue.get(full_child["id"])["status"], "checking")
        self.assertEqual(self.queue.get(degraded_child["id"])["status"], "checking")
        self.assertEqual(len(self.write_calls), 2)

        current = self.templates.view()
        self.templates.update_custom("full", CustomTemplateRequest(
            expected_version=current["version"], name=current["template_meta"]["full"]["name"],
            whitelist=["fixture-updated-full-model"], mappings=[],
        ))
        current = self.templates.view()
        self.templates.delete_custom("degraded", current["version"])

        await self.queue.run(full_child["id"])
        await self.queue.run(degraded_child["id"])

        self.assertEqual(self.queue.get(full_child["id"])["status"], "completed")
        self.assertEqual(self.queue.get(degraded_child["id"])["status"], "completed")
        self.assertEqual(len(self.write_calls), 2)
        self.assertEqual(self.db.rows[1]["model_mapping"], self.write_calls[0][1])
        self.assertEqual(self.db.rows[2]["model_mapping"], self.write_calls[1][1])

    async def test_batch_reports_unchanged_success_and_ineligible_children_together(self):
        full_target = {
            "fixture-full-model": "fixture-full-model",
            "fixture-input-*": "fixture-full-model",
        }
        self.db.rows[1]["model_mapping"] = copy.deepcopy(full_target)
        self.db.rows[1]["model_mapping_version"] = digest(full_target)
        self.db.rows[3]["platform"] = "fixture-other-platform"

        submitted = await self.queue.submit_template_batch(
            self.batch_request([1, 2, 3]), ADMIN_FIXTURE,
        )
        initial = {item["account_id"]: item for item in submitted["items"]}
        self.assertEqual(initial[1]["status"], "completed")
        self.assertTrue(initial[1]["result"]["unchanged"])
        self.assertEqual(initial[2]["status"], "queued")
        self.assertEqual(initial[3]["status"], "failed")
        self.assertEqual(submitted["pending"], 1)

        await self.queue.run(initial[2]["id"])

        history = self.queue.listing(batch_id=submitted["batch_id"])
        final = {item["account_id"]: item for item in history["items"]}
        self.assertEqual(final[1]["status"], "completed")
        self.assertTrue(final[1]["result"]["unchanged"])
        self.assertEqual(final[2]["status"], "completed")
        self.assertEqual(final[3]["status"], "failed")
        self.assertEqual(history["pending"], 0)
        self.assertEqual([call[0] for call in self.write_calls], [2])
        self.assertEqual(self.db.rows[2]["model_mapping"], full_target)

    async def test_other_template_and_name_changes_do_not_invalidate_queued_batch(self):
        submitted = await self.queue.submit_template_batch(self.batch_request([1]), ADMIN_FIXTURE)
        child = submitted["items"][0]
        before = copy.deepcopy(self.db.rows[1])

        current = self.templates.view()
        self.templates.update_custom("full", CustomTemplateRequest(
            expected_version=current["version"],
            name="fixture full renamed only",
            whitelist=current["templates"]["full"]["whitelist"],
            mappings=current["templates"]["full"]["mappings"],
        ))
        current = self.templates.view()
        self.templates.update_custom("degraded", CustomTemplateRequest(
            expected_version=current["version"], name="fixture degraded changed",
            whitelist=["fixture-new-degraded-model"], mappings=[],
        ))

        await self.queue.run(child["id"])

        self.assertEqual(self.queue.get(child["id"])["status"], "completed")
        self.assertEqual(self.write_calls, [(1, {
            "fixture-full-model": "fixture-full-model",
            "fixture-input-*": "fixture-full-model",
        }, ADMIN_FIXTURE)])
        after = self.db.rows[1]
        self.assertEqual(after["model_mapping"], self.write_calls[0][1])
        for key in before.keys() - {"model_mapping", "model_mapping_version"}:
            self.assertEqual(after[key], before[key], key)


if __name__ == "__main__":
    unittest.main()
