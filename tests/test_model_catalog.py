from __future__ import annotations

import copy
import json
import os
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.model_api import install_model_api
from app.model_catalog import ModelCatalogService, import_fields
from app.model_config_store import ModelConfigStore
from app.model_rules import admitted, composite_target, merge, revision, targets, transform, validate_overrides

BASE = {"slug": "gpt-6-astra", "display_name": "Astra", "description": "Original", "context_window": 400000,
        "max_context_window": 1000000, "input_modalities": ["text", "image"], "priority": 0,
        "supported_reasoning_levels": [{"effort": "low", "description": "Quick"}, {"effort": "high", "description": "Deep"}],
        "default_reasoning_level": "high", "supports_parallel_tool_calls": True,
        "model_messages": {"instructions_template": "native", "unknown": {"a": 1, "b": 2}}}


class RuleTests(unittest.TestCase):
    def test_recursive_merge_arrays_explicit_null_false_and_identity(self):
        patch = {"model_messages": {"unknown": {"a": 0}}, "supports_parallel_tool_calls": False,
                 "input_modalities": ["text"], "default_reasoning_level": None, "future": {"v": None}}
        value = merge(BASE, patch)
        self.assertEqual(value["model_messages"]["unknown"], {"a": 0, "b": 2})
        self.assertEqual(value["input_modalities"], ["text"])
        self.assertFalse(value["supports_parallel_tool_calls"])
        self.assertIsNone(value["default_reasoning_level"])
        self.assertEqual(BASE["input_modalities"], ["text", "image"])
        validate_overrides({BASE["slug"]: patch}, {BASE["slug"]: BASE})
        for field in ("slug", "id"):
            with self.assertRaises(ValueError): validate_overrides({"m": {field: "other"}})

    def test_type_context_and_effort_validation(self):
        for fields in ({"context_window": True}, {"context_window": 0}, {"context_window": 11, "max_context_window": 10},
                       {"input_modalities": []}, {"input_modalities": ["audio"]}, {"display_name": None},
                       {"priority": 1.5}, {"supports_parallel_tool_calls": "false"}, {"model_messages": None},
                       {"supported_reasoning_levels": [{"effort": "low"}], "default_reasoning_level": "high"},
                       {"effective_context_window_percent": 101}, {"truncation_policy": {"limit": False}}):
            with self.subTest(fields=fields), self.assertRaises(ValueError): validate_overrides({"m": fields})
        with self.assertRaises(ValueError): validate_overrides({"m": {"default_reasoning_level": "max"}}, {"m": BASE})
        with self.assertRaises(ValueError): validate_overrides({"m*": {}})
        with self.assertRaises(ValueError): validate_overrides({"m": {"future": float("nan")}})

    def test_transform_filters_drafts_and_preserves_envelope_and_native_fields(self):
        body = {"models": [BASE, {**BASE, "slug": "other"}], "future": {"x": True}}
        result = transform(body, {"gpt-6-astra": {"display_name": "Custom"}, "removed": {"priority": 1}}, {"enabled": True, "models": ["gpt-*"]})
        self.assertEqual(len(result["models"]), 1)
        self.assertEqual(result["models"][0]["display_name"], "Custom")
        self.assertEqual(result["models"][0]["model_messages"], BASE["model_messages"])
        self.assertEqual(result["future"], {"x": True})
        self.assertEqual(body["models"][0]["display_name"], "Astra")
        self.assertFalse(admitted({"enabled": True, "models": ["gpt-?"]}, "gpt-6"))
        with self.assertRaises(ValueError):
            transform(body, {"gpt-6-astra": {"max_context_window": 10}}, {"enabled": False, "models": []})

    def test_modelsdev_missing_fields_and_explicit_reasoning(self):
        self.assertEqual(import_fields({"id": "m", "name": "Name", "reasoning": True}), {"display_name": "Name"})
        fields = import_fields({"limit": {"context": 1000}, "reasoning_options": [{"type": "effort", "values": [None, "high"]}]})
        self.assertEqual(fields["default_reasoning_level"], "none")
        self.assertEqual(fields["max_context_window"], 1000)
        self.assertNotIn("input_modalities", fields)
        self.assertEqual(import_fields({"reasoning": False})["supported_reasoning_levels"], [{"effort": "none", "description": ""}])
        full = import_fields({**BASE, "future": 0})
        self.assertNotIn("slug", full)
        self.assertEqual(full["future"], 0)
        self.assertEqual(import_fields({"supported_reasoning_levels": ["low", "high"]})["supported_reasoning_levels"],
                         [{"effort": "low", "description": ""}, {"effort": "high", "description": ""}])

    def test_composite_exact_endpoint_priority_mapping_and_fixed_accounts(self):
        accounts = [{"id": 1, "platform": "openai", "status": "active", "schedulable": True,
                     "credentials": {"model_mapping": {"public": "real"}}}]
        routes = [{"id": 1, "enabled": True, "public_model": "alias", "match_type": "exact", "endpoint": "responses", "target_platform": "openai", "upstream_model": "public"}]
        group = {"platform": "composite"}
        self.assertEqual(targets(group, "alias", accounts, routes), [(accounts[0], "real")])
        self.assertEqual(targets({**group, "codex_models_manifest_config": {"enabled": True, "account_ids": [9]}}, "alias", accounts, routes), [])
        self.assertEqual(targets({**group, "codex_models_manifest_config": {"enabled": True, "account_ids": [9], "fallback_to_scheduler": True}}, "alias", accounts, routes), [(accounts[0], "real")])
        self.assertIsNone(composite_target("alias", [{**routes[0], "endpoint": "images"}], accounts))
        ambiguous = [*accounts, {**accounts[0], "platform": "grok"}]
        self.assertIsNone(composite_target("public", [], ambiguous))
        prefix = {**routes[0], "match_type": "prefix", "public_model": "al", "endpoint": "responses", "upstream_model": "right"}
        longer_any = {**prefix, "public_model": "alia", "endpoint": "any", "upstream_model": "wrong"}
        self.assertEqual(composite_target("alias", [longer_any, prefix], accounts), ("openai", "right"))


class StoreTests(unittest.TestCase):
    def test_atomic_permissions_conflict_restart_and_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "model.json"
            store = ModelConfigStore(str(path))
            initial = store.group(7)
            saved = store.save(7, {"m": {"display_name": "测试", "future": 0}}, initial["revision"])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(ModelConfigStore(str(path)).group(7), saved)
            self.assertEqual({p.name for p in path.parent.iterdir()}, {"model.json.lock", "model.json"})
            with self.assertRaises(HTTPException): store.save(7, {}, initial["revision"])
            path.write_text("{")
            before = path.read_bytes()
            with self.assertRaises(ValueError): store.save(7, {}, saved["revision"])
            self.assertEqual(path.read_bytes(), before)

    def test_parallel_stores_do_not_lose_another_group(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "model.json")
            with ThreadPoolExecutor(2) as pool:
                futures = [pool.submit(ModelConfigStore(path).save, i, {"m": {"priority": i}}, revision({})) for i in (1, 2)]
                [f.result() for f in futures]
            self.assertEqual(set(ModelConfigStore(path).read()["groups"]), {"1", "2"})


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.group = {"id": 7, "name": "Primary", "platform": "openai", "model_allowlist": {"enabled": False, "models": []}, "codex_models_manifest_config": {}, "updated_at": datetime.now(timezone.utc)}
        self.db = Mock()
        self.db.fetch_one.side_effect = lambda sql, params: {"group_id": 7 if params.get("key") == "caller-7" else 8} if "FROM api_keys" in sql else {**self.group, "id": params["id"]}
        self.db.fetch_all.return_value = []
        self.runtime = SimpleNamespace(settings=SimpleNamespace(model_config_path=str(Path(self.directory.name) / "config.json")),
                                       db=self.db, oauth_base_url=lambda: "http://127.0.0.1:12345")
        self.service = ModelCatalogService(self.runtime)
        self.addCleanup(self.service.close)
        self.requests = []
        self.response = {"models": [BASE]}
        self.status = 200
        self.service.read_http = self.native

    def native(self, method, url, **kwargs):
        self.requests.append((method, url, kwargs))
        return self.status, {"content-type": "application/json", "etag": '"native"'}, json.dumps(self.response).encode()

    def proxy(self, key="caller-7", **extra):
        return self.service.proxy("/v1/models", "client_version=0.116.0", {"authorization": "Bearer " + key, "x-real-ip": "127.0.0.1", **extra})

    def test_authentication_on_every_hit_group_isolation_etag_and_forwarded_identity(self):
        self.service.store.save(7, {"gpt-6-astra": {"display_name": "Seven"}}, revision({}))
        code, headers, raw = self.proxy()
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(raw)["models"][0]["display_name"], "Seven")
        self.assertEqual(self.proxy(**{"if-none-match": headers["etag"]})[0], 304)
        self.assertEqual(len(self.requests), 2)
        self.assertEqual(self.requests[0][2]["headers"]["authorization"], "Bearer caller-7")
        self.assertEqual(self.requests[0][2]["headers"]["x-real-ip"], "127.0.0.1")
        self.assertEqual(json.loads(self.proxy("caller-8")[2])["models"][0]["display_name"], "Astra")
        self.status = 401
        self.assertEqual(self.proxy(**{"if-none-match": headers["etag"]})[0], 401)
        self.assertEqual(len(self.requests), 4)

    def test_invalid_config_and_database_failure_return_unchanged_native(self):
        Path(self.runtime.settings.model_config_path).write_text("{")
        code, headers, raw = self.proxy()
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(raw), self.response)
        self.assertEqual(headers["x-sub2ops-catalog"], "fallback")
        self.assertEqual(self.service.status["state"], "fallback")
        self.db.fetch_one.side_effect = RuntimeError("PRIVATE_SECRET")
        self.assertEqual(json.loads(self.proxy()[2]), self.response)
        self.assertNotIn("PRIVATE_SECRET", json.dumps(self.service.status))

    def test_composite_adds_existing_route_alias_without_mutation(self):
        self.group["platform"] = "composite"
        account = {"id": 1, "platform": "openai", "type": "apikey", "status": "active", "schedulable": True, "credentials": {"model_mapping": {"public": "gpt-6-astra"}}}
        route = {"id": 1, "enabled": True, "match_type": "exact", "endpoint": "responses", "public_model": "alias", "target_platform": "openai", "upstream_model": "public"}
        self.db.fetch_all.side_effect = lambda sql, _: [route] if "composite_model_routes" in sql else [account]
        result = json.loads(self.proxy()[2])
        self.assertEqual([m["slug"] for m in result["models"]], ["gpt-6-astra", "alias", "public"])
        self.assertEqual(result["models"][1]["model_messages"], BASE["model_messages"])
        account["schedulable"] = False
        self.assertEqual([m["slug"] for m in json.loads(self.proxy()[2])["models"]], ["gpt-6-astra"])
        self.assertTrue(all(r[0] == "GET" for r in self.requests))
        self.db.execute.assert_not_called()

    def test_save_does_not_retarget_unavailable_override_or_overwrite_new_group_version(self):
        self.service.baseline = lambda group: ({"models": [BASE]}, "native")
        group = self.service.group(7)
        old = self.service.store.save(7, {"gone": {"display_name": "keep"}}, revision({}))
        payload = {"expected_version": group["version"], "expected_revision": old["revision"], "overrides": {"gone": {"display_name": "keep"}, "gpt-6-astra": {"display_name": "new"}}}
        self.service.save_overrides(7, payload)
        saved = self.service.store.group(7)
        self.assertIn("gone", saved["overrides"])
        payload.update(expected_revision=saved["revision"], overrides={"new-model": {"display_name": "bad"}})
        with self.assertRaises(HTTPException): self.service.save_overrides(7, payload)
        payload["expected_version"] = "x" * 64
        with self.assertRaises(HTTPException): self.service.save_overrides(7, payload)

    def test_allowlist_partial_update_and_readback(self):
        group = self.service.group(7)
        def admin(key, method, path, payload):
            self.assertEqual((method, path), ("PUT", "/groups/7"))
            self.assertEqual(set(payload), {"model_allowlist"})
            self.group["model_allowlist"] = payload["model_allowlist"]
        self.service.admin = admin
        result = self.service.save_allowlist(7, "admin", {"expected_version": group["version"], "allowlist": {"enabled": True, "models": [BASE["slug"]]}})
        self.assertTrue(result["model_allowlist"]["enabled"])
        with self.assertRaises(HTTPException): self.service.save_allowlist(7, "admin", {"expected_version": group["version"], "allowlist": {"enabled": False, "models": []}})

    def test_routes_require_admin_and_old_surface_is_absent(self):
        app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)
        desktop = SimpleNamespace(authenticate=lambda key, fresh: None if key == "admin" else (_ for _ in ()).throw(HTTPException(401)))
        service = install_model_api(app, self.runtime, desktop)
        self.addCleanup(service.close)
        service.list_groups = lambda: {"groups": [], "status": {"state": "ready"}}
        client = TestClient(app)
        self.addCleanup(client.close)
        self.assertEqual(client.get("/api/desktop/v1/model-groups").status_code, 401)
        self.assertEqual(client.get("/api/desktop/v1/model-groups", headers={"x-api-key": "admin"}).status_code, 200)
        self.assertEqual(client.get("/models").status_code, 404)
        self.assertEqual(client.get("/docs").status_code, 404)
        self.assertEqual(client.post("/api/desktop/v1/model-groups/7/preview", headers={"x-api-key": "admin"}, json={"extra": 1}).status_code, 422)

    def test_loopback_uses_real_get_and_preserves_unknown_json(self):
        received = []
        body = self.response
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_GET(self):
                received.append((self.path, self.headers.get("Authorization"), self.headers.get("X-Real-IP")))
                raw = json.dumps(body).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        try:
            self.runtime.oauth_base_url = lambda: f"http://127.0.0.1:{server.server_port}"
            self.service.read_http = ModelCatalogService.read_http.__get__(self.service)
            self.assertEqual(json.loads(self.proxy()[2]), body)
            self.assertEqual(received, [("/v1/models?client_version=0.116.0", "Bearer caller-7", "127.0.0.1")])
        finally:
            server.shutdown(); server.server_close(); thread.join()


if __name__ == "__main__": unittest.main()
