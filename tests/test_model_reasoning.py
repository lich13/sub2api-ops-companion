from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.model_api import install_model_api
from app.model_catalog import ModelCatalogService
from app.model_config_store import ModelConfigStore
from app.model_reasoning import complete_descriptor, forwarding, reasoning_fields, routing_binding
from app.model_rules import revision, transform

MODEL = "future-model"
DESCRIPTOR = {"slug": MODEL, "display_name": "Actual future model", "context_window": 123456,
              "input_modalities": ["text"], "model_messages": {"instructions_template": "actual", "keep": False},
              "supported_reasoning_levels": [{"effort": "low", "description": "Quick"}, {"effort": "max", "description": "Deep"}],
              "default_reasoning_level": "max", "unknown": {"zero": 0, "null": None}}


class ReasoningTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.path = Path(directory.name) / "model.json"
        self.group = {"id": 7, "name": "QA", "platform": "openai", "model_allowlist": {"enabled": False, "models": []},
                      "codex_models_manifest_config": {}, "updated_at": "2026-09-29"}
        self.account = {"id": 1, "platform": "openai", "type": "apikey", "status": "active", "schedulable": True,
                        "credentials": {"api_key": "secret-not-a-dto", "model_mapping": {MODEL: MODEL}}, "extra": {}}
        self.db = Mock()
        self.db.fetch_one.side_effect = lambda sql, params=None: copy.deepcopy(self.group)
        self.db.fetch_all.return_value = [self.account]
        self.runtime = SimpleNamespace(db=self.db, settings=SimpleNamespace(model_config_path=str(self.path)),
                                       oauth_base_url=lambda: "http://127.0.0.1:1")
        self.service = ModelCatalogService(self.runtime)
        self.addCleanup(self.service.close)
        self.service.baseline = Mock(return_value=({"models": []}, "native"))
        self.service.source_catalog = Mock(return_value={"models": [DESCRIPTOR]})
        self.service.native_version = Mock(return_value="0.2.10")
        self.service.admin = Mock(side_effect=self.native_write)
        self.writes = []

    def native_write(self, key, method, path, body):
        self.writes.append(body)
        self.group.update(body, updated_at=self.group["updated_at"] + ".1")
        return {}

    def payload(self, **changes):
        resolved = self.service.resolve_reasoning(7, "admin", {"model": MODEL, **changes})
        return {"model": MODEL, "efforts": resolved["efforts"], "default_effort": resolved["default_effort"],
                "expected_binding": resolved["binding"], "expected_version": resolved["group"]["version"],
                "expected_revision": resolved["revision"], **changes}

    def live(self, body=None):
        group, saved = self.service.group(7), self.service.store.group(7)
        patches, additions = self.service.active_reasoning(body or {"models": []}, group, saved, [self.account], [], "0.2.10")
        return transform(body or {"models": []}, patches, group["model_allowlist"], additions)

    def test_exact_model_own_descriptor_only_two_fields_and_old_models_unchanged(self):
        payload = self.payload(efforts=["low", "high"], default_effort="high")
        result = self.service.save_reasoning(7, "admin", payload)
        self.assertEqual(result["outcome"], "saved")
        original = {**DESCRIPTOR, "slug": "native-old"}
        live = self.live({"models": [original]})
        self.assertEqual(live["models"][0], original)
        actual = live["models"][1]
        self.assertEqual({k: v for k, v in actual.items() if k not in {"supported_reasoning_levels", "default_reasoning_level"}},
                         {k: v for k, v in DESCRIPTOR.items() if k not in {"supported_reasoning_levels", "default_reasoning_level"}})
        self.assertEqual([v["effort"] for v in actual["supported_reasoning_levels"]], ["low", "high"])
        self.assertEqual(self.writes, [])
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)
        self.db.execute.assert_not_called()

    def test_missing_source_no_guess_no_descriptor_and_unknown_version_stays_draft(self):
        self.service.source_catalog.return_value = {"data": [{"id": MODEL, "reasoning": True}]}
        resolved = self.service.resolve_reasoning(7, "admin", {"model": MODEL})
        self.assertEqual(resolved["efforts"], [])
        self.assertEqual(resolved["default_effort"], "")
        payload = self.payload(efforts=["low"], default_effort="low")
        self.assertEqual(self.service.save_reasoning(7, "admin", payload)["outcome"], "draft")
        self.assertEqual(self.live()["models"], [])
        self.service.native_version.return_value = "0.2.99"
        self.service.source_catalog.return_value = {"models": [DESCRIPTOR]}
        self.assertEqual(self.service.save_reasoning(7, "admin", self.payload())["outcome"], "draft")

    def test_validation_default_exact_identity_and_api_surface(self):
        for efforts, default in [([], ""), (["low"], "high"), (["low", "low"], "low"), ([{}], "low")]:
            with self.assertRaises(ValueError): reasoning_fields(efforts, default)
        with self.assertRaises(ValueError): self.service.resolve_reasoning(7, "admin", {"model": "gpt-*"})
        app = FastAPI()
        auth = Mock(side_effect=lambda key, fresh: None if key == "admin" else (_ for _ in ()).throw(HTTPException(401)))
        svc = install_model_api(app, self.runtime, SimpleNamespace(authenticate=auth))
        self.addCleanup(svc.close)
        svc.resolve_reasoning = self.service.resolve_reasoning
        with TestClient(app) as client:
            path = "/api/desktop/v1/model-groups/7/reasoning/resolve"
            self.assertEqual(client.post(path, json={"model": MODEL}).status_code, 401)
            data = client.post(path, json={"model": MODEL}, headers={"x-api-key": "admin"}).json()
            self.assertNotIn("_descriptor", data)
            self.assertNotIn("secret-not-a-dto", json.dumps(data))
            self.assertTrue(auth.call_args.kwargs["fresh"])
            self.assertEqual(client.post(path, json={"model": MODEL, "context_window": 999}, headers={"x-api-key": "admin"}).status_code, 422)
            for method, path in [("GET", "/model-catalog"), ("GET", "/model-groups/7"), ("PUT", "/model-groups/7/overrides"), ("PUT", "/model-groups/7/allowlist"), ("POST", "/model-groups/7/preview"), ("POST", "/model-groups/7/upstream-import")]:
                self.assertEqual(client.request(method, "/api/desktop/v1" + path).status_code, 404)

    def test_rechecking_prefilled_choices_keeps_their_actual_source(self):
        for source in ("upstream", "native"):
            with self.subTest(source=source):
                self.service.source_catalog.return_value = {"models": [DESCRIPTOR] if source == "upstream" else []}
                self.service.baseline.return_value = ({"models": [DESCRIPTOR]}, "native")
                initial = self.service.resolve_reasoning(7, "admin", {"model": MODEL})
                self.assertEqual(initial["source"], source)
                choices = {"model": MODEL, "efforts": initial["efforts"], "default_effort": initial["default_effort"]}
                rechecked = self.service.resolve_reasoning(7, "admin", choices)
                self.assertEqual(rechecked["source"], source)
                changed = self.service.resolve_reasoning(7, "admin", {**choices, "default_effort": "low"})
                self.assertEqual(changed["source"], "manual")
                self.assertFalse(self.path.exists())

    def test_whitelist_explicit_append_preserves_switch_and_partial_truth(self):
        self.group["model_allowlist"] = {"enabled": True, "models": ["old", "other-*"]}
        payload = self.payload()
        with self.assertRaises(HTTPException): self.service.save_reasoning(7, "admin", payload)
        self.assertFalse(self.writes)
        payload["confirm_allowlist"] = True
        saved = self.service.save_reasoning(7, "admin", payload)
        self.assertEqual(saved["outcome"], "saved")
        self.assertEqual(self.writes, [{"model_allowlist": {"enabled": True, "models": ["old", "other-*", MODEL]}}])
        self.service.remove_reasoning(7, "admin", {"model": MODEL, "expected_version": saved["state"]["group"]["version"], "expected_revision": saved["state"]["revision"]})
        self.assertIn(MODEL, self.group["model_allowlist"]["models"])
        self.group["model_allowlist"]["models"].remove(MODEL)
        self.service.store.save_reasoning = Mock(side_effect=OSError("PRIVATE"))
        result = self.service.save_reasoning(7, "admin", {**self.payload(), "confirm_allowlist": True})
        self.assertEqual(result["outcome"], "partial")
        self.assertIn(MODEL, self.group["model_allowlist"]["models"])
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_conflict_routing_change_and_native_restoration(self):
        stale = self.payload()
        self.group["updated_at"] = "changed"
        with self.assertRaises(HTTPException): self.service.save_reasoning(7, "admin", stale)
        saved = self.service.save_reasoning(7, "admin", self.payload())
        self.account["credentials"]["model_mapping"][MODEL] = "different-target"
        self.assertEqual(self.live()["models"], [])
        state = self.service.read_reasoning(7, "admin")
        self.assertEqual(state["items"][0]["state"], "unverified")
        self.account["credentials"]["model_mapping"][MODEL] = MODEL
        self.service.baseline.return_value = ({"models": [DESCRIPTOR]}, "native")
        self.assertEqual(self.service.read_reasoning(7, "admin")["items"][0]["state"], "native")
        native = self.service.save_reasoning(7, "admin", self.payload())
        self.assertEqual(native["outcome"], "native")
        self.assertEqual(native["state"]["items"], [])
        self.assertNotEqual(saved["state"]["revision"], native["state"]["revision"])

    def test_grok_limited_stays_draft_even_when_native_directory_matches(self):
        self.group["platform"] = self.account["platform"] = "grok"
        self.service.baseline.return_value = ({"models": [DESCRIPTOR]}, "native")
        result = self.service.save_reasoning(7, "admin", self.payload())
        self.assertEqual(result["outcome"], "draft")
        self.assertEqual(result["state"]["items"][0]["state"], "limited")
        self.assertEqual(self.live({"models": [DESCRIPTOR]}), {"models": [DESCRIPTOR]})

    def test_legacy_fields_kept_reasoning_requires_review_and_concurrent_store_conflict(self):
        old = self.service.store.save(7, {MODEL: {**reasoning_fields(["low"], "low"), "future": {"flag": False}}}, revision({}))
        self.assertEqual(self.service.read_reasoning(7, "admin")["items"][0]["state"], "unverified")
        result = self.service.save_reasoning(7, "admin", self.payload())
        saved = ModelConfigStore(str(self.path)).group(7)
        self.assertEqual(saved["overrides"][MODEL], {"future": {"flag": False}})
        with self.assertRaises(HTTPException): self.service.store.save_reasoning(7, MODEL, None, old["revision"])
        self.assertEqual(saved["reasoning"][MODEL]["mode"], "active")
        self.service.remove_reasoning(7, "admin", {"model": MODEL, "expected_version": result["state"]["group"]["version"], "expected_revision": saved["revision"]})
        self.assertEqual(self.service.store.group(7)["overrides"][MODEL], {"future": {"flag": False}})

    def test_composite_mapping_policy_and_version_contracts(self):
        route = {"enabled": True, "id": 1, "public_model": "alias", "match_type": "exact", "target_platform": "openai", "upstream_model": MODEL, "endpoint": "responses"}
        group = {**self.group, "platform": "composite"}
        binding, targets = routing_binding(group, "alias", [self.account], [route])
        self.assertEqual(targets[0][1], MODEL)
        self.assertEqual(forwarding(group, "alias", ["max"], targets, "0.2.10")["state"], "verified")
        limited = {**group, "reasoning_effort_mappings": [{"model": "alias", "match_type": "exact", "from": "max", "to": "high"}]}
        self.assertEqual(forwarding(limited, "alias", ["max"], targets, "0.2.10")["state"], "limited")
        self.assertNotEqual(binding, routing_binding(limited, "alias", [self.account], [route])[0])
        self.assertEqual(forwarding({**group, "max_reasoning_effort": "high"}, "alias", ["max"], targets, "0.2.10")["state"], "limited")
        self.assertEqual(forwarding(group, "alias", ["max"], targets, "future")["state"], "unverified")
        other = {**self.account, "id": 2, "credentials": {"model_mapping": {MODEL: "other"}}}
        ambiguous = routing_binding(group, "alias", [self.account, other], [route])[1]
        self.assertEqual(forwarding(group, "alias", ["high"], ambiguous, "0.2.10")["state"], "unverified")
        for target, efforts, expected in [("grok-4.7", ["xhigh"], "verified"), ("grok-4.7", ["max"], "limited"), ("grok-4.5", ["xhigh"], "limited"), ("grok-future", ["high"], "limited")]:
            account = {**self.account, "platform": "grok"}
            self.assertEqual(forwarding(self.group, target, efforts, [(account, target)], "0.2.10")["state"], expected)
        self.assertFalse(complete_descriptor({"slug": MODEL}, MODEL))
        self.assertFalse(complete_descriptor(DESCRIPTOR, "other"))

    def test_protocol_change_invalidates_binding_and_does_not_claim_conversion_support(self):
        before, _ = routing_binding(self.group, MODEL, [self.account], [])
        self.account["extra"]["openai_responses_supported"] = False
        binding, available = routing_binding(self.group, MODEL, [self.account], [])
        self.assertNotEqual(binding, before)
        self.assertEqual(forwarding(self.group, MODEL, ["max"], available, "0.2.10")["state"], "unverified")
        self.account["extra"]["openai_responses_mode"] = "force_responses"
        self.assertEqual(forwarding(self.group, MODEL, ["max"], available, "0.2.10")["state"], "verified")

    def test_group_policy_matches_native_case_and_scope_precedence(self):
        rules = [{"from": "max", "to": "high"}, {"from": "max", "to": "max", "model": MODEL.upper(), "match_type": "exact"}]
        group = {**self.group, "reasoning_effort_mappings": rules}
        targets = [(self.account, MODEL)]
        self.assertEqual(forwarding(group, MODEL, ["max"], targets, "0.2.10")["state"], "verified")
        rules[1]["to"] = "deny"
        self.assertEqual(forwarding(group, MODEL, ["max"], targets, "0.2.10")["state"], "limited")

    def test_native_restore_removes_only_legacy_reasoning_fields(self):
        self.service.store.save(7, {MODEL: {**reasoning_fields(["low", "max"], "max"), "description": "keep"}}, revision({}))
        self.service.baseline.return_value = ({"models": [DESCRIPTOR]}, "native")
        result = self.service.save_reasoning(7, "admin", self.payload())
        self.assertEqual(result["outcome"], "native")
        self.assertEqual(result["state"]["items"], [])
        self.assertEqual(self.service.store.group(7)["overrides"][MODEL], {"description": "keep"})


if __name__ == "__main__":
    unittest.main()
