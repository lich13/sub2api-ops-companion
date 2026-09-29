"""Contract integration against a disposable native Sub2API, PG and catalog.

Catalog reads and explicit forwarding checks use synthetic identities only.
The runner refuses non-disposable databases and captures no production traffic.
"""
from __future__ import annotations

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse
from urllib.request import Request, urlopen
from urllib.error import HTTPError

import psycopg
from psycopg.types.json import Jsonb

DB = os.environ["DATABASE_URL"]
if os.environ.get("DESKTOP_QA_DISPOSABLE") != "sub2ops-desktop" or urlparse(DB).hostname != "pg":
    raise SystemExit("Refusing: disposable QA database required")
ADMIN = "desktop-isolated-test-key"
NATIVE = "http://sub2api:8080"
OPS = "http://127.0.0.1:18081"
MODELS = ["gpt-6-astra", "gpt-6-sol", "gpt-5.6-luna", "gpt-qa-upstream", "grok-qa-future"]
NEW = "gpt-qa-future"


def request(url, method="GET", body=None, headers=None):
    req = Request(url, method=method, headers={"Content-Type": "application/json", **(headers or {})},
                  data=json.dumps(body).encode() if body is not None else None)
    try:
        with urlopen(req, timeout=60) as response:
            return response.status, dict(response.headers), json.loads(response.read() or b"{}")
    except HTTPError as error:
        raw = error.read()
        try:
            body = json.loads(raw or b"{}")
        except ValueError:
            body = {"non_json": True}
        return error.code, dict(error.headers), body


def admin(path, method="GET", body=None):
    return request(OPS + "/api/desktop/v1" + path, method, body, {"x-api-key": ADMIN})


def directory(key, path="/v1/models", **headers):
    return request(OPS + path + "?client_version=0.146.0", headers={"Authorization": "Bearer " + key, **headers})


def admin_fixture():
    # This synthetic readiness record belongs only to the disposable fixture.
    # No agreement endpoint is invoked and no production identity is involved.
    with psycopg.connect(DB) as db:
        user = db.execute("SELECT id FROM users WHERE email='desktop-qa@example.invalid'").fetchone()[0]
        db.execute("INSERT INTO settings(key,value) VALUES (%s,%s) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                   (f"admin_compliance_acknowledgement:{user}", json.dumps({"version": "v2026.06.10", "fixture": True})))


def seed():
    with psycopg.connect(DB) as db:
        assert db.execute("SELECT count(*) FROM accounts").fetchone()[0] == 0
        status, _, login = request(NATIVE + "/api/v1/auth/login", "POST", {"email": "desktop-qa@example.invalid", "password": "Desktop-QA-only-9264"})
        assert status == 200, (status, login)
        # Key generation is a native step-up protected operation. Fixture setup
        # injects only a disposable identity; all subsequent requests use the
        # unchanged native admin authentication middleware.
        db.execute("INSERT INTO settings(key,value) VALUES ('admin_api_key',%s)", (ADMIN,))
        user = db.execute("SELECT id FROM users WHERE email='desktop-qa@example.invalid'").fetchone()[0]
        db.execute("UPDATE users SET balance=100 WHERE id=%s", (user,))
        groups = []
        for name, platform in (("Catalog A", "openai"), ("Catalog B", "openai"), ("Catalog Composite", "composite"), ("Catalog Grok", "grok"), ("Catalog Limited", "openai")):
            groups.append(db.execute("INSERT INTO groups(name,platform,model_allowlist) VALUES (%s,%s,%s) RETURNING id",
                                    (name, platform, Jsonb({"enabled": False, "models": []}))).fetchone()[0])
        account = db.execute("INSERT INTO accounts(name,platform,type,credentials) VALUES ('Catalog account','openai','apikey',%s) RETURNING id",
                             (Jsonb({"base_url": "http://mock:18082", "api_key": "qa-upstream-only", "model_mapping": {**{model: model for model in MODELS if not model.startswith("grok")}, NEW: "gpt-qa-upstream"}}),)).fetchone()[0]
        grok = db.execute("INSERT INTO accounts(name,platform,type,credentials) VALUES ('Grok fixture','grok','apikey',%s) RETURNING id",
                          (Jsonb({"base_url": "http://mock:18082", "api_key": "qa-grok-only", "model_mapping": {"grok-qa-future": "grok-qa-future"}}),)).fetchone()[0]
        for index, (group, key) in enumerate(zip(groups, ("sk-catalog-a", "sk-catalog-b", "sk-catalog-composite", "sk-catalog-grok", "sk-catalog-limited"))):
            db.execute("INSERT INTO account_groups(account_id,group_id) VALUES (%s,%s)", (grok if index == 3 else account, group))
            db.execute("INSERT INTO api_keys(user_id,key,name,group_id) VALUES (%s,%s,'Catalog QA',%s)", (user, key, group))
        db.execute("INSERT INTO composite_model_routes(group_id,public_model,match_type,target_platform,upstream_model,endpoint,priority,enabled) VALUES (%s,'qa-alias','exact','openai','gpt-qa-future','responses',0,true)", (groups[2],))
        db.execute("UPDATE groups SET reasoning_effort_mappings=%s WHERE id=%s",
                   (Jsonb([{"from": "max", "to": "high"}]), groups[4]))
        db.execute("UPDATE groups SET model_allowlist=%s WHERE id=%s",
                   (Jsonb({"enabled": True, "models": ["gpt-6-astra"]}), groups[0]))
    admin_fixture()
    print(json.dumps({"seeded": True, "groups": groups}))


def verify():
    code, _, listing = admin("/model-groups"); assert code == 200, (code, listing)
    groups = {g["name"]: g["id"] for g in listing["groups"]}
    a, b, composite, grok, limited = (groups[n] for n in ("Catalog A", "Catalog B", "Catalog Composite", "Catalog Grok", "Catalog Limited"))
    with psycopg.connect(DB) as db:
        before = db.execute("SELECT id,credentials,extra,priority,schedulable,updated_at FROM accounts ORDER BY id").fetchall()
    version = request(NATIVE + "/api/v1/admin/system/version", headers={"x-api-key": ADMIN})[2]["data"]["version"]
    assert version.lstrip("v") == "0.2.10", version
    def resolve(gid, model, efforts=None):
        payload = {"model": model}
        if efforts:
            payload.update(efforts=efforts, default_effort=efforts[-1])
        code, _, data = admin(f"/model-groups/{gid}/reasoning/resolve", "POST", payload)
        assert code == 200, (code, data)
        assert "_descriptor" not in data and "qa-upstream-only" not in json.dumps(data)
        return data
    def save(gid, data, **extra):
        payload = {"model": data["model"], "efforts": data["efforts"], "default_effort": data["default_effort"],
                   "expected_binding": data["binding"], "expected_version": data["group"]["version"], "expected_revision": data["revision"], **extra}
        return admin(f"/model-groups/{gid}/reasoning", "PUT", payload)
    code, _, state = admin(f"/model-groups/{a}/reasoning")
    assert code == 200 and state["items"] == [], (code, state)
    native_before = directory("sk-catalog-a")[2]
    imported = resolve(a, NEW)
    assert imported["efforts"] == ["high"] and imported["default_effort"] == "high" and imported["source"] == "upstream", imported
    found = resolve(a, NEW, ["low", "high", "max"])
    assert found["needs_allowlist"] and found["descriptor_available"] and found["forwarding"]["state"] == "verified", found
    assert save(a, found)[0] == 409
    code, _, saved = save(a, found, confirm_allowlist=True)
    assert code == 200 and saved["outcome"] == "saved", (code, saved)
    assert save(a, found, confirm_allowlist=True)[0] == 409
    code, headers, live = directory("sk-catalog-a"); assert code == 200, (code, live)
    model = next(m for m in live["models"] if m["slug"] == NEW)
    assert [v["effort"] for v in model["supported_reasoning_levels"]] == ["low", "high", "max"], model
    assert model["default_reasoning_level"] == "max"
    original = request(NATIVE + "/v1/models?client_version=0.146.0", headers={"Authorization": "Bearer sk-catalog-a"})[2]
    native_model = next((m for m in original["models"] if m["slug"] == NEW), None)
    if native_model:
        reasoning_keys = {"supported_reasoning_levels", "default_reasoning_level"}
        assert {k: v for k, v in model.items() if k not in reasoning_keys} == {k: v for k, v in native_model.items() if k not in reasoning_keys}
    else:
        assert model["context_window"] == 400000 and model["unknown_extension"] == {"supported": False}
    old = next(m for m in live["models"] if m["slug"] == "gpt-6-astra")
    assert old == next(m for m in native_before["models"] if m["slug"] == "gpt-6-astra")
    assert directory("sk-catalog-a", **{"If-None-Match": headers.get("etag", headers.get("ETag"))})[0] == 304
    other = directory("sk-catalog-b")[2]
    assert not any(m["slug"] == NEW and m.get("default_reasoning_level") == "max" for m in other["models"]), other
    assert directory("invalid-key")[0] == 401
    alias = resolve(composite, "qa-alias", ["low", "max"])
    assert alias["forwarding"]["state"] == "verified", alias
    assert save(composite, alias)[2]["outcome"] == "saved"
    alias_model = next(m for m in directory("sk-catalog-composite")[2]["models"] if m["slug"] == "qa-alias")
    assert alias_model["default_reasoning_level"] == "max"
    grok_draft = resolve(grok, "grok-qa-future", ["low", "high"])
    assert grok_draft["forwarding"]["state"] == "limited", grok_draft
    assert save(grok, grok_draft)[2]["outcome"] == "draft"
    policy = resolve(limited, NEW, ["high", "max"])
    assert policy["forwarding"]["state"] == "limited", policy
    assert save(limited, policy)[2]["outcome"] == "draft"
    with psycopg.connect(DB) as db:
        assert db.execute("SELECT id,credentials,extra,priority,schedulable,updated_at FROM accounts ORDER BY id").fetchall() == before
        allowlist = db.execute("SELECT model_allowlist FROM groups WHERE id=%s", (a,)).fetchone()[0]
        assert allowlist == {"enabled": True, "models": ["gpt-6-astra", NEW]}, allowlist
    capture = request("http://mock:18082/qa-counts")[2]
    assert capture["model_calls"] == 0, capture
    for method, path in (("GET", "/model-catalog"), ("GET", f"/model-groups/{a}"), ("POST", f"/model-groups/{a}/preview"), ("PUT", f"/model-groups/{a}/overrides"), ("PUT", f"/model-groups/{a}/allowlist")):
        assert admin(path, method, {} if method != "GET" else None)[0] == 404, path

    # Only this block makes model requests, against the isolated mock. Check
    # what the unchanged native program actually sends, not just the catalog.
    cases = [("sk-catalog-a", NEW, "max", "gpt-qa-upstream", "max"),
             ("sk-catalog-composite", "qa-alias", "max", "gpt-qa-upstream", "max"),
             ("sk-catalog-grok", "grok-qa-future", "high", "grok-qa-future", None),
             ("sk-catalog-limited", NEW, "max", "gpt-qa-upstream", "high")]
    for key, model_id, effort, upstream, expected in cases:
        code, _, response = request(NATIVE + "/v1/responses", "POST",
                                    {"model": model_id, "input": "isolated fixture", "stream": False, "reasoning": {"effort": effort}},
                                    {"Authorization": "Bearer " + key})
        assert code == 200, (model_id, code, response)
        last = request("http://mock:18082/qa-counts")[2]["calls"][-1]
        assert last["model"] == upstream and last["effort"] == expected, last

    code, _, current = admin(f"/model-groups/{a}/reasoning")
    assert code == 200
    code, _, removed = admin(f"/model-groups/{a}/reasoning", "DELETE",
                             {"model": NEW, "expected_version": current["group"]["version"], "expected_revision": current["revision"]})
    assert code == 200 and removed["items"] == []
    with psycopg.connect(DB) as db:
        assert db.execute("SELECT model_allowlist FROM groups WHERE id=%s", (a,)).fetchone()[0] == allowlist
    edge = "http://127.0.0.1:18084"
    for path in ("/v1/models", "/models", "/backend-api/codex/models"):
        code, headers, response = request(edge + path + "?client_version=0.146.0", headers={"Authorization": "Bearer sk-catalog-a"})
        assert code == 200 and "x-sub2ops-catalog" in headers, (path, code, response)
        assert request(edge + path + "?client_version=0.146.0", headers={"Authorization": "Bearer invalid"})[0] == 401
    for path in ("/sub2ops", "/sub2ops/ops", "/sub2ops/sso/start", "/sub2ops/static/style.css", "/docs"):
        assert request(edge + path)[0] == 404, path
    assert request(edge + "/sub2ops/healthz")[0] == 200
    print(json.dumps({"catalog_integration": "passed", "native_version": version, "native_auth": True, "cross_group_isolation": True,
                      "ordinary_new_model": True, "composite": True, "etag": True, "catalog_model_calls": 0,
                      "explicit_mock_forwarding_cases": len(cases), "account_config_writes": 0}))


def serve():
    paths, calls = [], []
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_GET(self):
            if self.path == "/qa-counts":
                body = {"paths": paths, "model_calls": len(calls), "calls": calls}
            elif "models" in self.path:
                paths.append(self.path)
                body = {"models": [{"slug": model, "display_name": model, "description": "QA directory",
                          "context_window": 400000, "max_context_window": 1000000, "input_modalities": ["text", "image"],
                          "supported_reasoning_levels": [{"effort": "high", "description": "High"}], "default_reasoning_level": "high",
                          "model_messages": {"instructions_template": "QA own model instructions"},
                          "unknown_extension": {"supported": False}} for model in MODELS]}
            else:
                calls.append(self.path); self.send_response(404); self.end_headers(); return
            raw = json.dumps(body).encode(); self.send_response(200); self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
            calls.append({"path": self.path, "model": payload.get("model"), "effort": (payload.get("reasoning") or {}).get("effort", payload.get("reasoning_effort"))})
            response = {"id": "resp_qa", "object": "response", "created_at": 1790000000, "status": "completed",
                        "model": payload.get("model"), "output": [{"type": "message", "role": "assistant", "id": "msg_qa", "status": "completed",
                        "content": [{"type": "output_text", "text": "QA success", "annotations": []}]}],
                        "usage": {"input_tokens": 4, "output_tokens": 3, "total_tokens": 7}}
            if "chat/completions" in self.path:
                response = {"id": "chatcmpl_qa", "object": "chat.completion", "created": 1790000000, "model": payload.get("model"),
                            "choices": [{"index": 0, "message": {"role": "assistant", "content": "QA success"}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 4, "completion_tokens": 3, "total_tokens": 7}}
            if payload.get("stream"):
                event = {"type": "response.completed", "response": response}
                raw = ("event: response.completed\\ndata: " + json.dumps(event) + "\\n\\ndata: [DONE]\\n\\n").encode()
                content_type = "text/event-stream"
            else:
                raw, content_type = json.dumps(response).encode(), "application/json"
            self.send_response(200); self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
    ThreadingHTTPServer(("0.0.0.0", 18082), Handler).serve_forever()


if __name__ == "__main__": {"seed": seed, "verify": verify, "serve": serve}[sys.argv[1]]()
