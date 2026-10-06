from __future__ import annotations
import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock
from fastapi import HTTPException
from app.account_templates import AccountTemplates, TemplateApplication, combine, split

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

if __name__ == "__main__": unittest.main()
