"""Contract integration against a disposable native Sub2API, PG and catalog.

All upstream paths except model directories are rejected and counted. This
runner never uses production identities and refuses non-disposable databases.
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
MODELS = ["gpt-6-astra", "gpt-6-sol", "gpt-5.6-luna"]


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
        for name, platform in (("Catalog A", "openai"), ("Catalog B", "openai"), ("Catalog Composite", "composite")):
            groups.append(db.execute("INSERT INTO groups(name,platform,model_allowlist) VALUES (%s,%s,%s) RETURNING id",
                                    (name, platform, Jsonb({"enabled": False, "models": []}))).fetchone()[0])
        account = db.execute("INSERT INTO accounts(name,platform,type,credentials) VALUES ('Catalog account','openai','apikey',%s) RETURNING id",
                             (Jsonb({"base_url": "http://mock:18082", "api_key": "qa-upstream-only", "model_mapping": {model: model for model in MODELS}}),)).fetchone()[0]
        for group, key in zip(groups, ("sk-catalog-a", "sk-catalog-b", "sk-catalog-composite")):
            db.execute("INSERT INTO account_groups(account_id,group_id) VALUES (%s,%s)", (account, group))
            db.execute("INSERT INTO api_keys(user_id,key,name,group_id) VALUES (%s,%s,'Catalog QA',%s)", (user, key, group))
        db.execute("INSERT INTO composite_model_routes(group_id,public_model,match_type,target_platform,upstream_model,endpoint,priority,enabled) VALUES (%s,'qa-alias','exact','openai','gpt-6-astra','responses',0,true)", (groups[2],))
    admin_fixture()
    print(json.dumps({"seeded": True, "groups": groups}))


def verify():
    code, _, listing = admin("/model-groups"); assert code == 200, (code, listing)
    groups = {g["name"]: g["id"] for g in listing["groups"]}
    a, b, composite = (groups[name] for name in ("Catalog A", "Catalog B", "Catalog Composite"))
    with psycopg.connect(DB) as db:
        before = db.execute("SELECT id,credentials,extra,priority,schedulable,updated_at FROM accounts ORDER BY id").fetchall()
    code, _, state = admin(f"/model-groups/{a}"); assert code == 200, (code, state)
    assert state["baseline_status"] == "native", state
    assert "gpt-6-astra" in [m["slug"] for m in state["baseline"]["models"]]
    changes = {"gpt-6-astra": {"display_name": "QA custom", "supports_parallel_tool_calls": False,
                "priority": 0, "future_extension": {"zero": 0, "nothing": None}}}
    payload = {"expected_version": state["group"]["version"], "expected_revision": state["revision"], "overrides": changes}
    code, _, saved = admin(f"/model-groups/{a}/overrides", "PUT", payload); assert code == 200, (code, saved)
    assert admin(f"/model-groups/{a}/overrides", "PUT", payload)[0] == 409
    code, headers, live = directory("sk-catalog-a"); assert code == 200, (code, live)
    model = next(m for m in live["models"] if m["slug"] == "gpt-6-astra")
    assert model["display_name"] == "QA custom" and model["supports_parallel_tool_calls"] is False
    assert model["future_extension"] == {"zero": 0, "nothing": None}
    assert directory("sk-catalog-a", **{"If-None-Match": headers.get("etag", headers.get("ETag"))})[0] == 304
    code, _, other = directory("sk-catalog-b"); assert code == 200
    assert next(m for m in other["models"] if m["slug"] == "gpt-6-astra")["display_name"] != "QA custom"
    assert directory("invalid-key")[0] == 401
    assert admin(f"/model-groups/{a}/preview", "POST", {"allowlist": {"enabled": True, "models": ["gpt-6-sol"]}, "overrides": changes})[2]["effective"]["models"][0]["slug"] == "gpt-6-sol"
    code, _, imported = admin(f"/model-groups/{a}/upstream-import", "POST", {"model": "gpt-6-astra"})
    assert code == 200 and imported["fields"]["unknown_extension"] == {"supported": False}, (code, imported)
    assert "qa-upstream-only" not in json.dumps(imported)
    code, _, composite_body = directory("sk-catalog-composite"); assert code == 200, (code, composite_body)
    assert "qa-alias" in [m["slug"] for m in composite_body["models"]], composite_body
    code, _, saved_list = admin(f"/model-groups/{a}/allowlist", "PUT", {"expected_version": state["group"]["version"], "allowlist": {"enabled": True, "models": ["gpt-6-sol"]}})
    assert code == 200, (code, saved_list)
    code, _, narrowed = directory("sk-catalog-a"); assert code == 200
    assert [m["slug"] for m in narrowed["models"]] == ["gpt-6-sol"], narrowed
    assert admin(f"/model-groups/{a}/allowlist", "PUT", {"expected_version": state["group"]["version"], "allowlist": {"enabled": False, "models": []}})[0] == 409
    with psycopg.connect(DB) as db:
        assert db.execute("SELECT id,credentials,extra,priority,schedulable,updated_at FROM accounts ORDER BY id").fetchall() == before
    capture = request("http://mock:18082/qa-counts")[2]
    assert capture["model_calls"] == 0, capture
    assert all("models" in path for path in capture["paths"]), capture
    for path in ("/", "/ops", "/sso/start", "/static/style.css", "/docs", "/openapi.json"):
        assert request(OPS + path)[0] == 404
    edge = "http://127.0.0.1:18084"
    auth = {"Authorization": "Bearer sk-catalog-a"}
    for path in ("/v1/models", "/models", "/backend-api/codex/models"):
        code, headers, response = request(edge + path + "?client_version=0.146.0", headers=auth)
        assert code == 200 and "gpt-6-sol" in [m["slug"] for m in response["models"]], (path, code, response)
        assert headers.get("x-sub2ops-catalog") == "active", headers
        assert request(edge + path + "?client_version=0.146.0", headers={"Authorization": "Bearer invalid"})[0] == 401
    for path in ("/v1/models", "/models"):
        code, headers, response = request(edge + path, headers=auth)
        assert code == 200 and "data" in response and "x-sub2ops-catalog" not in headers, (path, code)
    for path in ("/sub2ops", "/sub2ops/", "/sub2ops/ops", "/sub2ops/sso/start", "/sub2ops/static/style.css"):
        assert request(edge + path)[0] == 404, path
    assert request(edge + "/sub2ops/healthz")[0] == 200
    print(json.dumps({"catalog_integration": "passed", "native_auth": True, "cross_group_isolation": True,
                      "composite": True, "etag": True, "upstream_directory_reads": len(capture["paths"]), "model_calls": 0, "account_writes": 0}))


def serve():
    paths, calls = [], []
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_GET(self):
            if self.path == "/qa-counts":
                body = {"paths": paths, "model_calls": len(calls)}
            elif "models" in self.path:
                paths.append(self.path)
                body = {"models": [{"slug": model, "display_name": model, "description": "QA directory",
                          "context_window": 400000, "max_context_window": 1000000, "input_modalities": ["text", "image"],
                          "supported_reasoning_levels": [{"effort": "high", "description": "High"}], "default_reasoning_level": "high",
                          "unknown_extension": {"supported": False}} for model in MODELS]}
            else:
                calls.append(self.path); self.send_response(404); self.end_headers(); return
            raw = json.dumps(body).encode(); self.send_response(200); self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
        def do_POST(self):
            calls.append(self.path); self.send_response(405); self.end_headers()
    ThreadingHTTPServer(("0.0.0.0", 18082), Handler).serve_forever()


if __name__ == "__main__": {"seed": seed, "verify": verify, "serve": serve}[sys.argv[1]]()
