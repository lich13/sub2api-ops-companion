from __future__ import annotations
import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch
from fastapi import HTTPException
from pydantic import ValidationError
from app.account_templates import AccountTemplates, CustomTemplateDeleteRequest, CustomTemplateRequest, TemplateApplication, TemplatesRequest, combine, split

class _DB:
    def __init__(self):
        self.rows = {
            1: {"id": 1, "name": "fixture-full", "platform": "openai", "type": "oauth", "deleted_at": None, "parent_account_id": None, "passthrough": False, "model_mapping": {"fixture-full-model": "fixture-full-model", "fixture-full-alt": "fixture-full-alt", "fixture-full-third": "fixture-full-third"}},
            2: {"id": 2, "name": "fixture-degraded", "platform": "openai", "type": "oauth", "deleted_at": None, "parent_account_id": None, "passthrough": False, "model_mapping": {"fixture-model": "fixture-model", "fixture-input-*": "fixture-model", "fixture-terra-*": "fixture-model"}},
        }
    def fetch_one(self, _sql, params): return copy.deepcopy(self.rows.get(params["id"]))
    def fetch_all(self, _sql, _params=None): return copy.deepcopy(list(self.rows.values()))

class AccountTemplateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); root = Path(self.tmp.name); self.db = _DB()
        self.service = SimpleNamespace(invalidate=Mock(), r=SimpleNamespace(db=self.db, settings=SimpleNamespace(usage_query_state_path=str(root / "oauth.json"), audit_path=str(root / "audit.jsonl"))))
        self.templates = AccountTemplates(self.service)
    def tearDown(self): self.tmp.cleanup()
    def test_combine_and_split_native_semantics(self):
        value = {"whitelist": ["fixture-model"], "mappings": [{"source": "fixture-input-*", "target": "fixture-model"}]}
        self.assertEqual(combine(value), {"fixture-model": "fixture-model", "fixture-input-*": "fixture-model"})
        self.assertEqual(split(combine(value)), value); self.assertEqual(combine(split({})), {})
        with self.assertRaises(HTTPException): combine({"whitelist": ["fixture-*"]})
    def test_initialization_does_not_guess_source_accounts(self):
        view = self.templates.initialize_sources()
        self.assertFalse(view["configured"])
    def test_initialization_copies_only_three_model_profiles(self):
        view = self.templates.initialize_sources(1, 2)
        self.assertEqual(view["templates"]["full"]["whitelist"], ["fixture-full-model", "fixture-full-alt", "fixture-full-third"])
        self.assertEqual(view["templates"]["degraded"]["mappings"], [{"source": "fixture-input-*", "target": "fixture-model"}, {"source": "fixture-terra-*", "target": "fixture-model"}])
        self.assertEqual(view["templates"]["takeover"], view["templates"]["degraded"]); self.db.rows[1]["model_mapping"] = {}
        self.assertEqual(self.templates.view()["templates"]["full"]["whitelist"], ["fixture-full-model", "fixture-full-alt", "fixture-full-third"]); self.assertNotIn("credentials", json.dumps(view)); self.assertEqual(self.templates.store.path.stat().st_mode & 0o777, 0o600)
    def test_initialization_is_idempotent_without_nested_lock(self):
        first = self.templates.initialize_sources(1, 2); second = self.templates.initialize_sources(1, 2); self.assertEqual(first["version"], second["version"])
    def test_desired_and_application_are_versioned(self):
        view = self.templates.initialize_sources(1, 2); payload = TemplateApplication(expected_version="0" * 64, template_id="degraded", template_version=view["version"])
        self.assertEqual(self.templates.desired(payload), {"fixture-model": "fixture-model", "fixture-input-*": "fixture-model", "fixture-terra-*": "fixture-model"}); self.assertTrue(self.templates.achieved(2, payload))
        with self.assertRaises(HTTPException): self.templates.desired(TemplateApplication(expected_version="0" * 64, template_id="degraded", template_version="1" * 64))

    def test_migrates_legacy_template_state_with_three_builtin_profiles(self):
        old_templates = {
            "full": {"whitelist": ["fixture-old-full"], "mappings": []},
            "degraded": {"whitelist": ["fixture-old-degraded"], "mappings": []},
            "takeover": {"whitelist": [], "mappings": []},
        }
        self.templates.store.path.write_text(json.dumps({
            "version": 1, "revision": 4, "configured": True, "templates": old_templates,
        }))

        view = self.templates.view()

        self.assertEqual(view["templates"], old_templates)
        self.assertEqual(view["custom_templates"], [])
        stored = json.loads(self.templates.store.path.read_text())
        self.assertEqual(stored["revision"], 5)
        self.assertEqual(stored["custom_templates"], [])

    def test_custom_template_crud_uses_dynamic_id_and_preserves_legacy_put(self):
        initial = self.templates.view()
        created = self.templates.create_custom(CustomTemplateRequest(
            expected_version=initial["version"],
            name="fixture custom",
            whitelist=["fixture-custom-model"],
            mappings=[{"source": "fixture-custom-*", "target": "fixture-custom-model"}],
        ))
        custom = created["custom_templates"][0]
        self.assertRegex(custom["id"], r"^custom-[a-f0-9]{24}$")
        payload = TemplateApplication(expected_version="0" * 64, template_id=custom["id"],
                                      template_version=created["version"])
        self.assertEqual(self.templates.desired(payload), {
            "fixture-custom-model": "fixture-custom-model",
            "fixture-custom-*": "fixture-custom-model",
        })

        updated = self.templates.update_custom(custom["id"], CustomTemplateRequest(
            expected_version=created["version"],
            name="fixture custom edited",
            whitelist=["fixture-edited-model"],
            mappings=[],
        ))
        self.assertEqual(updated["custom_templates"][0]["name"], "fixture custom edited")
        self.assertEqual(self.templates.desired(TemplateApplication(
            expected_version="0" * 64, template_id=custom["id"], template_version=updated["version"],
        )), {"fixture-edited-model": "fixture-edited-model"})

        legacy_put = TemplatesRequest(
            expected_version=updated["version"],
            full={"whitelist": ["fixture-full-updated"], "mappings": []},
            degraded={"whitelist": ["fixture-degraded-updated"], "mappings": []},
            takeover={"whitelist": [], "mappings": []},
        )
        saved = self.templates.save(legacy_put)
        self.assertEqual(saved["templates"]["full"]["whitelist"], ["fixture-full-updated"])
        self.assertEqual(saved["custom_templates"][0]["id"], custom["id"])
        with self.assertRaises(HTTPException) as stale:
            self.templates.delete_custom(custom["id"], CustomTemplateDeleteRequest(expected_version=updated["version"]))
        self.assertEqual(stale.exception.status_code, 409)

        deleted = self.templates.delete_custom(custom["id"], CustomTemplateDeleteRequest(expected_version=saved["version"]))
        self.assertEqual(deleted["custom_templates"], [])
        with self.assertRaises(HTTPException) as missing:
            self.templates.desired(TemplateApplication(
                expected_version="0" * 64, template_id=custom["id"], template_version=deleted["version"],
            ))
        self.assertEqual(missing.exception.status_code, 422)

    def test_custom_templates_are_capped_at_32_total(self):
        view = self.templates.view()
        for index in range(29):
            view = self.templates.create_custom(CustomTemplateRequest(
                expected_version=view["version"],
                name=f"fixture {index}",
                whitelist=[],
                mappings=[],
            ))

        self.assertEqual(len(view["templates"]), 32)
        self.assertEqual(len(view["custom_templates"]), 29)
        with self.assertRaises(HTTPException) as limit:
            self.templates.create_custom(CustomTemplateRequest(
                expected_version=view["version"],
                name="fixture over limit",
                whitelist=[],
                mappings=[],
            ))
        self.assertEqual(limit.exception.status_code, 409)
        self.assertEqual(len(self.templates.view()["templates"]), 32)

    def test_custom_template_names_reject_whitespace_and_over_limit(self):
        version = self.templates.view()["version"]
        with self.assertRaises(HTTPException) as blank:
            self.templates.create_custom(CustomTemplateRequest(
                expected_version=version,
                name=" \t ",
                whitelist=[],
                mappings=[],
            ))
        self.assertEqual(blank.exception.status_code, 422)

        with self.assertRaises(ValidationError):
            CustomTemplateRequest(
                expected_version=version,
                name="x" * 41,
                whitelist=[],
                mappings=[],
            )
        self.assertEqual(self.templates.view()["custom_templates"], [])

    def test_builtin_templates_cannot_be_deleted(self):
        version = self.templates.view()["version"]
        for template_id in ("full", "degraded", "takeover"):
            with self.subTest(template_id=template_id):
                with self.assertRaises(HTTPException) as missing:
                    self.templates.delete_custom(template_id, version)
                self.assertEqual(missing.exception.status_code, 404)
        self.assertEqual(set(self.templates.view()["templates"]), {"full", "degraded", "takeover"})

    def test_legacy_migration_save_error_is_propagated(self):
        old_templates = {
            "full": {"whitelist": ["fixture-old-full"], "mappings": []},
            "degraded": {"whitelist": ["fixture-old-degraded"], "mappings": []},
            "takeover": {"whitelist": [], "mappings": []},
        }
        self.templates.store.path.write_text(json.dumps({
            "version": 1, "revision": 4, "configured": True, "templates": old_templates,
        }))

        with patch("app.policy_store.write_json", side_effect=OSError("fixture migration write failure")):
            with self.assertRaisesRegex(OSError, "fixture migration write failure"):
                self.templates.data()

        self.assertNotIn("custom_templates", json.loads(self.templates.store.path.read_text()))

if __name__ == "__main__": unittest.main()
