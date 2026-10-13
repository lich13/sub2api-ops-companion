#!/usr/bin/env python3
"""Fixture-only HTTP contracts and Linux/Docker connection-probe measurements.

Examples (run from the repository, using the existing virtualenv):
  .venv/bin/python scripts/connection_probe_check.py --mode contract \
    --implementation python --database-url "$PROBE_TEST_DATABASE_URL"
  .venv/bin/python scripts/connection_probe_check.py --mode contract \
    --implementation rust --command '["experiments/connection_probe/rust/target/release/sub2ops-probe"]' \
    --database-url "$PROBE_TEST_DATABASE_URL"
  python scripts/connection_probe_check.py --mode benchmark --implementation rust \
    --image sub2ops-probe-rust --binary-path /usr/local/bin/sub2ops-probe \
    --database-url "$PROBE_TEST_DATABASE_URL"

The PostgreSQL URL must resolve to loopback and allow CREATE DATABASE. The runner
creates and drops only its own randomly named database. With --image and no URL,
it can start its own loopback-only postgres:16 container. It never reads a real
account database. Temporary state, child processes and containers are removed.
Only one JSON result is written to stdout; progress goes to stderr. Benchmark
POST/GET percentiles are measured HTTP samples, with their counts reported.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import os
import platform
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from dataclasses import dataclass
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import ProxyHandler, Request, build_opener


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from app.operation_versions import versions  # noqa: E402

ADMIN = "fixture-admin-connection-039"
CLIENT = "fixture-client-connection-039"
MODEL = "fixture-probe-model"
MAPPED = "fixture-native-mapped-model"
GROK = "fixture-grok-native-model"
CONTENT = "fixture-response-content-must-not-persist-039"
BEARER = "fixture-bearer-must-not-persist-039"
PROVIDER = "fixture-provider-must-not-persist-039"
ACCOUNT_CREDENTIAL = "fixture-account-credential-must-not-persist-039"
API = "/api/desktop/v1"
RUNNING = {"queued", "running", "checking"}
PENDING = RUNNING | {"needs_confirmation"}
RESULT_FIELDS = {"requested_model", "actual_model", "success", "duration_ms",
                 "completed_at", "error", "uncertain"}


class CheckFailure(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise CheckFailure(message)


def progress(message):
    print(message, file=sys.stderr, flush=True)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def percentile(values, fraction):
    ordered = sorted(values)
    if not ordered:
        return None
    index = (len(ordered) - 1) * fraction
    lo, hi = math.floor(index), math.ceil(index)
    return round(ordered[lo] + (ordered[hi] - ordered[lo]) * (index - lo), 3)


def distribution(values):
    return {"samples": len(values), "p50": percentile(values, .5), "p95": percentile(values, .95)}


def shell(command, *, timeout=45, check=True):
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        # Do not echo argv: environment arguments can contain a database URL.
        raise CheckFailure(f"{Path(command[0]).name} exited {result.returncode}")
    return result


def wait_until(predicate, message, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.02)
    raise CheckFailure(message)


def rows_for_fixture(special=False):
    rows = {aid: {"id": aid, "name": f"fixture-account-{aid}", "platform": "openai",
        "type": "oauth", "status": "active", "schedulable": True, "deleted_at": None,
        "parent_account_id": None, "credential_version": f"fixture-version-{aid}",
        "proxy_id": None, "group_ids": [13], "model_catalog_version": "fixture-catalog",
        "model_mapping": {}, "model_mapping_version": "fixture-mapping", "priority": 1,
        "extra": {}, "credentials": {"access_token": ACCOUNT_CREDENTIAL},
        "group_allowlists": [{"enabled": True, "models": ["fixture-unrelated-model"]}]}
        for aid in range(1, 1002)}
    if special:
        rows[1]["model_mapping"] = {MODEL: MAPPED}
        rows[2].update(platform="grok", type="apikey")
        rows[3]["type"] = "apikey"
        rows[4]["platform"] = "anthropic"
        rows[5]["deleted_at"] = "fixture-deleted"
        rows[6]["type"] = "fixture-unsupported"
        rows[7].update(group_ids=[31, 2, 13], credential_version="fixture-版本", proxy_id=17)
        for aid in (8, 9):
            rows[aid].update(parent_account_id=1, extra={"parent_account_id": 1})
    return rows


class FixtureDatabase:
    def __init__(self, admin_url, *, docker=False, postgres_image="postgres:16"):
        self.admin_url, self.docker, self.postgres_image = admin_url, docker, postgres_image
        self.name = "probe_fixture_" + uuid.uuid4().hex
        self.container = None
        self.created = False
        self.url = None
        self.rows = {}

    def __enter__(self):
        try:
            import psycopg
            from psycopg import sql
            from psycopg.conninfo import conninfo_to_dict, make_conninfo
            from psycopg.types.json import Jsonb
        except ImportError as exc:
            raise CheckFailure("psycopg is required; use the repository virtualenv") from exc
        self.pg, self.sql, self.Jsonb = psycopg, sql, Jsonb
        try:
            if not self.admin_url:
                require(self.docker, "provide --database-url or PROBE_TEST_DATABASE_URL for command mode")
                port = free_port()
                self.container = "probe-pg-" + uuid.uuid4().hex
                shell(["docker", "run", "--detach", "--name", self.container, "--network", "host",
                    "--label", "sub2ops.probe.fixture=true", "--env", "POSTGRES_USER=probe_fixture",
                    "--env", "POSTGRES_PASSWORD=fixture-postgres", "--env", "POSTGRES_DB=probe_fixture",
                    self.postgres_image, "-c", "listen_addresses=127.0.0.1", "-c", f"port={port}"], timeout=60)
                self.admin_url = make_conninfo(host="127.0.0.1", port=port, user="probe_fixture",
                    password="fixture-postgres", dbname="probe_fixture", connect_timeout=2)
            config = conninfo_to_dict(self.admin_url)
            require(config.get("host", "") in {"localhost", "127.0.0.1", "::1"},
                    "fixture PostgreSQL host must be explicitly loopback")
            require(config.get("hostaddr", "127.0.0.1") in {"127.0.0.1", "::1"},
                    "fixture PostgreSQL hostaddr must be loopback")
            require(not config.get("service") and not config.get("options"),
                    "fixture URL must not contain service or options overrides")
            self.admin_url = make_conninfo(self.admin_url, connect_timeout=2)
            def ready():
                try:
                    with psycopg.connect(self.admin_url, autocommit=True):
                        return True
                except psycopg.OperationalError:
                    return False
            wait_until(ready, "fixture PostgreSQL unavailable on loopback", timeout=35 if self.container else 4)
            with psycopg.connect(self.admin_url, autocommit=True) as connection:
                connection.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(self.name)))
            self.created = True
            # Both services deliberately accept only a loopback postgresql URL.
            host = config["host"]
            host = "[::1]" if host == "::1" else host
            user = quote(config.get("user", "probe_fixture"), safe="")
            password = quote(config.get("password", ""), safe="")
            credentials = user + (":" + password if password else "")
            self.url = f"postgresql://{credentials}@{host}:{config.get('port', '5432')}/{self.name}"
            with psycopg.connect(self.url, autocommit=True) as connection:
                connection.execute("CREATE TABLE probe_accounts(id bigint PRIMARY KEY, row_data jsonb NOT NULL)")
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def reset(self, special=False):
        self.rows = rows_for_fixture(special)
        with self.pg.connect(self.url) as connection:
            connection.execute("TRUNCATE probe_accounts")
            with connection.cursor() as cursor:
                cursor.executemany("INSERT INTO probe_accounts(id,row_data) VALUES (%s,%s)",
                    [(aid, self.Jsonb(row)) for aid, row in self.rows.items()])
        return self.rows

    def update(self, aid, **changes):
        self.rows[aid].update(changes)
        with self.pg.connect(self.url) as connection:
            connection.execute("UPDATE probe_accounts SET row_data=%s WHERE id=%s",
                               (self.Jsonb(self.rows[aid]), aid))

    def full_schema(self):
        with self.pg.connect(self.url) as connection:
            connection.execute((ROOT / "tests/fixtures/connection_probe.sql").read_text())
            for table in ("accounts", "groups", "account_groups", "usage_logs", "ops_error_logs"):
                count = connection.execute(self.sql.SQL("SELECT count(*) FROM {}").format(self.sql.Identifier(table))).fetchone()[0]
                require(count == 0, "full runtime reference must have empty fixture tables")

    def __exit__(self, *_args):
        errors = []
        if self.created:
            try:
                with self.pg.connect(self.admin_url, autocommit=True) as connection:
                    connection.execute(self.sql.SQL("DROP DATABASE {} WITH (FORCE)").format(self.sql.Identifier(self.name)))
                self.created = False
            except Exception:
                errors.append("owned fixture database cleanup failed")
        if self.container:
            result = shell(["docker", "rm", "--force", self.container], check=False)
            if result.returncode:
                errors.append("owned PostgreSQL container cleanup failed")
            self.container = None
        if errors:
            raise CheckFailure("; ".join(errors))


class NativeMock:
    def __init__(self, rows, delay=.01):
        self.rows, self.delay = rows, delay
        self.scenarios, self.gates, self.calls = {}, {}, []
        self.active, self.conflicts, self.unexpected = {}, [], []
        self.max_active = self.auth_calls = self.auth_denied = 0
        self.authorized = True
        self.lock, self.closing = threading.Lock(), threading.Event()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def reply(self, status, value):
                encoded = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def do_GET(self):
                if self.path == "/api/v1/admin/groups/all":
                    valid = self.headers.get("x-api-key") == ADMIN and fixture.authorized
                    with fixture.lock:
                        fixture.auth_calls += 1
                        fixture.auth_denied += int(not valid)
                    self.reply(200 if valid else 401, {"code": 0 if valid else 401, "data": []})
                else:
                    with fixture.lock:
                        fixture.unexpected.append(["GET", self.path])
                    self.reply(503, {"code": 503, "message": "fixture catalog unavailable"})

            def do_POST(self):
                match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)/test", self.path)
                if not match:
                    with fixture.lock:
                        fixture.unexpected.append(["POST", self.path])
                    self.reply(404, {"code": 404})
                    return
                aid, token = int(match[1]), uuid.uuid4().hex
                try:
                    body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                except (ValueError, TypeError):
                    self.reply(400, {"code": 400})
                    return
                row = fixture.rows.get(aid, {})
                resources = {aid, row.get("parent_account_id")} - {None}
                with fixture.lock:
                    for other_aid, other_resources in fixture.active.values():
                        if resources & other_resources:
                            fixture.conflicts.append([aid, other_aid])
                    fixture.active[token] = (aid, resources)
                    fixture.max_active = max(fixture.max_active, len(fixture.active))
                    fixture.calls.append({"account_id": aid, "body": body,
                        "authorized": self.headers.get("x-api-key") == ADMIN, "started": time.monotonic()})
                try:
                    scenario = fixture.scenarios.get(aid, "success")
                    if scenario == "disconnect":
                        self.close_connection = True
                        return
                    if scenario == "http_error":
                        self.reply(503, {"code": 503, "message": CONTENT})
                        return
                    requested = body.get("model_id", "")
                    actual = row.get("model_mapping", {}).get(requested, requested or GROK)
                    events = [{"type": "test_start", "model": actual},
                        {"type": "content", "text": CONTENT}, {"type": "status", "text": CONTENT},
                        {"type": "image", "image_url": "data:image/png;base64," + CONTENT}]
                    if scenario == "no_model":
                        events = [event for event in events if event["type"] != "test_start"]
                    prefix = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    if scenario == "truncated":
                        self.send_header("Content-Length", str(len(prefix) + 1000))
                    self.end_headers()
                    self.wfile.write(prefix)
                    self.wfile.flush()
                    gate = fixture.gates.get(aid)
                    while gate is not None and not gate.wait(.02) and not fixture.closing.is_set():
                        pass
                    if scenario == "timeout":
                        # Retire the simulated native request when the client
                        # times out, so peak-concurrency counts live sockets.
                        while not fixture.closing.wait(.02):
                            if select.select([self.connection], [], [], 0)[0]:
                                if not self.connection.recv(1, socket.MSG_PEEK):
                                    break
                        return
                    if scenario in {"truncated", "no_terminal"}:
                        self.close_connection = True
                        return
                    if fixture.closing.wait(fixture.delay):
                        return
                    event = ({"type": "error", "error": f"provider: Bearer {BEARER}; api_key={PROVIDER}"}
                        if scenario == "native_error" else {"type": "test_complete", "success": scenario != "failure"})
                    self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                finally:
                    with fixture.lock:
                        fixture.active.pop(token, None)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": .02}, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def block(self, ids):
        for aid in ids:
            self.gates[aid] = threading.Event()

    def release(self):
        for gate in self.gates.values():
            gate.set()

    def called(self):
        with self.lock:
            return [call["account_id"] for call in self.calls]

    def assert_native_only(self):
        require(not self.unexpected, "connection probe requested catalog or another non-native route")
        require(all(call["authorized"] for call in self.calls), "native test omitted fixture admin authorization")
        require(all(not ({"confirmed", "expected_version"} & set(call["body"])) for call in self.calls),
                "native payload leaked local confirmation/version fields")
        require(self.max_active <= 2, "native concurrency exceeded two")

    def __exit__(self, *_args):
        self.closing.set()
        self.release()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


@dataclass
class Response:
    status: int
    body: object
    milliseconds: float


def request(url, path, method="GET", body=None, key=ADMIN, timeout=15):
    headers = {"Content-Type": "application/json"}
    if key is not None:
        headers["x-api-key"] = key
    payload = None if body is None else json.dumps(body).encode()
    started = time.monotonic()
    opener = build_opener(ProxyHandler({}))
    try:
        response = opener.open(Request(url + path, data=payload, headers=headers, method=method), timeout=timeout)
    except HTTPError as exc:
        response = exc
    with response:
        encoded = response.read()
        status = response.status
    elapsed = (time.monotonic() - started) * 1000
    try:
        decoded = json.loads(encoded) if encoded else None
    except ValueError:
        decoded = {"non_json_response_bytes": len(encoded)}
    return Response(status, decoded, elapsed)


class ProbeService:
    def __init__(self, args, database, upstream, state_dir, *, full=False, stream_timeout=None):
        self.args, self.database, self.upstream = args, database, upstream
        self.state_dir, self.full = state_dir, full
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.image = args.full_image if full else args.image
        self.container, self.process, self.log = None, None, None
        self.log_path = state_dir.parent / (state_dir.name + ".log")
        target_state = "/probe-state" if self.image else str(state_dir)
        self.env = {"SUB2OPS_PROBE_ISOLATED": "fixture-only", "PROBE_DATABASE_URL": database.url,
            "PROBE_STATE_DIR": target_state, "PROBE_UPSTREAM": upstream.url, "PROBE_PORT": str(self.port),
            "PROBE_MODEL": MODEL, "PROBE_CONCURRENCY": "2", "PROBE_ADMIN_KEY": ADMIN,
            "PROBE_TIMEOUT_SECONDS": str(stream_timeout or args.stream_timeout),
            "PYTHONDONTWRITEBYTECODE": "1", "PYTHONUNBUFFERED": "1"}
        self.command = args.full_command if full else args.command
        if full:
            substitutions = {"database_url": database.url, "state_dir": target_state,
                "upstream": upstream.url, "port": str(self.port)}
            for key, value in args.full_env.items():
                for name, replacement in substitutions.items():
                    value = value.replace("{" + name + "}", replacement)
                self.env[key] = value
            # These bindings are mandatory even if optional full-env is used.
            self.env.update(DATABASE_URL=database.url, APP_PORT=str(self.port), BASE_PATH="",
                SUB2API_BASE_URL=upstream.url, SUB2API_VERIFY_BASE_URL="", BARK_ENABLED="false",
                OAUTH_RECOVERY_MONITOR_ENABLED="false", OAUTH_DAILY_TEST_ENABLED="false",
                OAUTH_AUTO_RESET_CREDIT_ENABLED="false", BARK_DEVICE_KEY="")
            for key, filename in {"AUDIT_PATH": "audit.jsonl", "USAGE_QUERY_STATE_PATH": "usage-query-state.json",
                "OAUTH_CONFIG_PATH": "oauth-config.json", "BARK_CONFIG_PATH": "bark-config.json",
                "KEY_FALLBACK_CONFIG_PATH": "key-fallback-config.json", "SUB2API_CONFIG_PATH": "sub2api-config.json",
                "GROUP_MODEL_CONFIG_PATH": "group-model-config.json"}.items():
                self.env[key] = target_state + "/" + filename
            if not self.command:
                self.command = ["python", "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(self.port)]
        elif not self.image and not self.command:
            self.command = ([sys.executable, "-m", "experiments.connection_probe.python_slice"]
                if args.implementation == "python" else [str(ROOT / "experiments/connection_probe/rust/target/release/sub2ops-probe")])
        if self.command:
            self.command = [part.replace("{port}", str(self.port)) for part in self.command]

    def start(self):
        if self.image:
            if self.container:
                shell(["docker", "start", self.container])
            else:
                self.container = "probe-svc-" + uuid.uuid4().hex
                command = ["docker", "run", "--detach", "--init", "--name", self.container,
                    "--network", "host", "--label", "sub2ops.probe.fixture=true",
                    "--user", f"{os.getuid()}:{os.getgid()}",
                    "--mount", f"type=bind,src={self.state_dir},dst=/probe-state"]
                for key, value in self.env.items():
                    command.extend(["--env", key + "=" + value])
                command.append(self.image)
                command.extend(self.command or [])
                shell(command)
        else:
            self.log = self.log_path.open("ab")
            self.process = subprocess.Popen(self.command, cwd=ROOT, env={**os.environ, **self.env},
                stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True)
        def ready():
            if self.process and self.process.poll() is not None:
                raise CheckFailure(f"probe process exited {self.process.returncode} before healthz")
            try:
                return request(self.url, "/healthz", key=None, timeout=.5).status == 200
            except (URLError, TimeoutError, ConnectionError):
                return False
        try:
            wait_until(ready, "probe /healthz did not become ready", timeout=30)
        except Exception as exc:
            raise CheckFailure(safe_error(exc) + "; " + self.diagnostics()) from exc

    def diagnostics(self):
        if self.container:
            output = shell(["docker", "logs", "--tail", "100", self.container], check=False)
            text = output.stdout + output.stderr
        else:
            text = self.log_path.read_text(errors="replace") if self.log_path.exists() else ""
        categories = sorted(set(re.findall(r"\b[A-Z][A-Za-z]+(?:Error|Exception)\b", text)))
        missing = sorted(set(re.findall(r'(?:column|relation) "([A-Za-z0-9_.]+)" does not exist', text)))
        return "service_diagnostics=" + json.dumps({"exception_types": categories, "missing_fixture_fields": missing})

    def crash(self):
        if self.container:
            shell(["docker", "kill", "--signal", "KILL", self.container])
        elif self.process:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait(timeout=5)
            self.process = None
            self.log.close()
            self.log = None

    def close(self):
        errors = []
        if self.container:
            result = shell(["docker", "rm", "--force", self.container], check=False)
            if result.returncode:
                errors.append("owned probe container cleanup failed")
            self.container = None
        if self.process:
            if self.process.poll() is None:
                os.killpg(self.process.pid, signal.SIGTERM)
                try:
                    self.process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    os.killpg(self.process.pid, signal.SIGKILL)
                    self.process.wait(timeout=4)
            self.process = None
        if self.log:
            self.log.close()
            self.log = None
        if errors:
            raise CheckFailure("; ".join(errors))


class Session:
    def __init__(self, args, database, temp_root, name, *, special=False, delay=.01, stream_timeout=None):
        self.args, self.db = args, database
        self.rows = database.reset(special)
        self.mock = NativeMock(self.rows, delay)
        self.state_dir = temp_root / name
        self.state_dir.mkdir(mode=0o700)
        self.service = ProbeService(args, database, self.mock, self.state_dir, stream_timeout=stream_timeout)
        self.public = []

    def __enter__(self):
        self.mock.__enter__()
        try:
            self.service.start()
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def api(self, path, method="GET", body=None, key=ADMIN, expected=200):
        response = request(self.service.url, API + path, method, body, key)
        self.public.append(response.body)
        require(response.status == expected, f"{method} {path.split('?')[0]} returned {response.status}, expected {expected}")
        return response

    def payload(self, ids, *, rid=None, expected=None):
        expected = expected or {}
        return {"action": "test", "payload": {"confirmed": True}, "accounts": [
            {"account_id": aid, "expected_version": expected.get(aid, versions(self.rows[aid])["test"] if aid in self.rows else "a" * 64)}
            for aid in ids], "request_id": rid or "fixture-request-" + uuid.uuid4().hex, "client_id": CLIENT}

    def submit(self, ids, **kwargs):
        return self.post(self.payload(ids, **kwargs)).body

    def post(self, body, expected=202):
        response = self.api("/account-operations/batch", "POST", body, expected=expected)
        if expected == 202:
            require(isinstance(response.body, dict) and re.fullmatch("[a-f0-9]{32}", response.body.get("batch_id", "")),
                    "accepted batch lacks hexadecimal batch_id")
            require(len(response.body.get("items", [])) == len(body["accounts"]), "accepted batch item count differs")
        return response

    def listing(self, bid):
        return self.api("/account-operations?batch_id=" + bid)

    def settle(self, bid, *, latencies=None, timeout=30):
        def settled():
            response = self.listing(bid)
            if latencies is not None:
                latencies.append(response.milliseconds)
            body = response.body
            items = body["items"]
            require(body["pending"] == sum(item["status"] in PENDING for item in items), "pending count disagrees with item states")
            return body if all(item["status"] not in RUNNING for item in items) else None
        return wait_until(settled, "batch still has queued/running/checking items", timeout)

    def cancel(self, bid):
        return self.api("/account-operations/batches/" + bid + "/cancel", "POST", {}).body

    def privacy(self):
        public = json.dumps(self.public, ensure_ascii=False).encode()
        surfaces = [("HTTP response", public)]
        for path in self.state_dir.rglob("*"):
            if path.is_file():
                surfaces.append(("persisted " + path.name, path.read_bytes()))
        for name, surface in surfaces:
            for marker in (ADMIN, CONTENT, BEARER, PROVIDER, ACCOUNT_CREDENTIAL):
                require(marker.encode() not in surface, f"{name} retained a forbidden fixture marker")
        self.mock.assert_native_only()

    def __exit__(self, *_args):
        try:
            self.service.close()
        finally:
            self.mock.__exit__(None, None, None)


def summary(item, *, requested=MODEL, actual=MODEL, success=True, uncertain=False):
    result = item.get("result")
    require(isinstance(result, dict), "completed/uncertain native test lacks result")
    require(set(result) <= RESULT_FIELDS, "native result retained fields outside the summary contract")
    require(result.get("requested_model") == requested, "requested_model differs from frozen batch configuration")
    require(result.get("actual_model") == actual, "actual_model differs from native test_start")
    require(result.get("success") is success, "success differs from native terminal result")
    require(type(result.get("duration_ms")) is int and result["duration_ms"] >= 0, "duration_ms must be a nonnegative integer")
    require(datetime.fromisoformat(result["completed_at"].replace("Z", "+00:00")).tzinfo is not None,
            "completed_at lacks timezone")
    if uncertain:
        require(result.get("uncertain") is True and item["status"] == "needs_confirmation", "uncertain result must require confirmation")
    if not success:
        require(bool(result.get("error")), "unsuccessful result lacks an error summary")


def contract_health(session):
    session.api("/accounts", key=None, expected=401)
    session.api("/accounts", key="fixture-wrong-key", expected=401)
    items = session.api("/accounts").body["items"]
    require(len(items) >= 1000, "fixture account listing is incomplete")
    seen = set()
    for item in items:
        aid = item["id"]
        require(aid in session.rows and aid not in seen, "account listing contains missing/duplicate fixture identity")
        seen.add(aid)
        require(item["operation_versions"]["test"] == versions(session.rows[aid])["test"],
                f"account {aid} test version differs from real Python versions(row)")
    require({1, 2, 7} <= seen, "account listing omitted required version vectors")
    session.privacy()
    return {"version_vectors": len(items), "unauthorized_status": 401}


def contract_native(session):
    batch = session.submit([1, 2, 3, 4, 5, 6, 7, 2001], expected={7: "f" * 64})
    final = session.settle(batch["batch_id"])
    items = {item["account_id"]: item for item in final["items"]}
    summary(items[1], actual=MAPPED)
    summary(items[2], requested="", actual=GROK)
    summary(items[3])
    require({items[aid]["status"] for aid in (4, 5, 6, 2001)} == {"failed"}, "ineligible/missing accounts did not fail separately")
    require(items[7]["status"] == "needs_confirmation" and final["pending"] == 1, "stale version was not retained for confirmation")
    require(sorted(session.mock.called()) == [1, 2, 3], "unsupported or stale account reached native upstream")
    for call in session.mock.calls:
        grok = call["account_id"] == 2
        require(call["body"].get("model_id") == ("" if grok else MODEL), "native model selection differs by platform")
        require(call["body"].get("mode") == ("text" if grok else "default"), "native mode differs by platform")
    session.privacy()
    return {"native_calls": 3, "catalog_requests": 0, "pending_confirmation": 1}


def contract_idempotency(session):
    session.mock.block([10, 11])
    body = session.payload([10, 11])
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
        responses = list(executor.map(lambda _: session.post(body).body, range(2)))
    bid = responses[0]["batch_id"]
    require(responses[1]["batch_id"] == bid, "concurrent retry created a second batch")
    reordered = {**body, "accounts": list(reversed(body["accounts"]))}
    require(session.post(reordered).body["batch_id"] == bid, "order-only retry created another batch")
    for changed in ({**body, "accounts": body["accounts"][:1]}, {**body, "client_id": "fixture-other-client-039"},
                    {**body, "accounts": [{**body["accounts"][0], "expected_version": "f" * 64}, body["accounts"][1]]}):
        session.post(changed, expected=409)
    session.post(session.payload([12, 12]), expected=422)
    session.mock.release()
    session.settle(bid)
    require(sorted(session.mock.called()) == [10, 11], "idempotency retry dispatched extra native requests")
    session.privacy()
    return {"one_durable_batch": True, "conflicts_rejected": 3, "native_calls": 2}


def contract_limits(session):
    valid = session.payload([10])
    invalid = [{**valid, "payload": value} for value in ({"confirmed": False}, {"confirmed": "true"},
        {"confirmed": True, "model_id": MODEL}, {})]
    invalid += [{**valid, "accounts": []}, {**valid, "request_id": "short"},
        {**valid, "client_id": "short"}, {**valid, "unexpected": True},
        {**valid, "accounts": [{"account_id": True, "expected_version": "a" * 64}]}]
    for body in invalid:
        session.post(body, expected=422)
    session.post(session.payload(range(1, 1002)), expected=422)
    require(not session.mock.called(), "rejected payload dispatched a native request")
    session.mock.block(range(1, 1001))
    accepted = session.post(session.payload(range(1, 1001)))
    cancelled = session.cancel(accepted.body["batch_id"])
    require(any(item["status"] == "cancelled" for item in cancelled["items"]), "1000-item batch had no cancellable queued item")
    session.mock.release()
    final = session.settle(accepted.body["batch_id"])
    require(len(final["items"]) == 1000 and final["pending"] == 0, "1000-item batch lost items during cancellation")
    session.privacy()
    return {"accepted": 1000, "rejected": 1001, "acceptance_ms": round(accepted.milliseconds, 3), "invalid_payloads": len(invalid)}


def contract_cancel(session):
    ids = list(range(10, 16))
    session.mock.block(ids)
    batch = session.submit(ids)
    wait_until(lambda: len(session.mock.called()) == 2, "two native slots were not occupied")
    dispatched = set(session.mock.called())
    cancelled = session.cancel(batch["batch_id"])
    require({item["account_id"] for item in cancelled["items"] if item["status"] == "cancelled"} == set(ids) - dispatched,
            "cancel did not affect exactly the undispatched items")
    session.mock.release()
    final = session.settle(batch["batch_id"])
    require(set(session.mock.called()) == dispatched and len(session.mock.called()) == 2, "cancelled work reached native upstream")
    require(final["pending"] == 0, "cancelled batch retained pending work")
    session.privacy()
    return {"running_preserved": 2, "queued_cancelled": 4}


def contract_locks(session):
    session.mock.block([1, 10])
    first = session.submit([1, 8, 9, 10])
    second = session.submit([10])
    wait_until(lambda: {1, 10} <= set(session.mock.called()), "independent account did not run alongside parent")
    require(session.mock.called().count(10) == 1, "same account ran twice while its first request was blocked")
    session.mock.release()
    session.settle(first["batch_id"])
    session.settle(second["batch_id"])
    require(sorted(session.mock.called()) == [1, 8, 9, 10, 10], "parent/sibling work did not dispatch once each")
    require(not session.mock.conflicts, "same account or shared parent ran concurrently")
    session.privacy()
    return {"native_calls": 5, "peak_concurrency": session.mock.max_active, "lock_conflicts": 0}


def contract_revalidation(session):
    session.mock.block([10, 11])
    batch = session.submit([10, 11, 12, 13])
    wait_until(lambda: len(session.mock.called()) == 2, "two initial probes did not start")
    session.db.update(12, credential_version="fixture-replaced-version")
    session.db.update(13, extra={"passive_usage": 42}, updated_at="fixture-passive-observation")
    session.mock.release()
    items = {item["account_id"]: item for item in session.settle(batch["batch_id"])["items"]}
    require(items[12]["status"] == "needs_confirmation", "credential change before dispatch was not revalidated")
    summary(items[13])
    require(sorted(session.mock.called()) == [10, 11, 13], "changed credential reached native test")
    session.privacy()
    return {"changed_version_blocked": True, "passive_change_allowed": True}


def contract_failures(session):
    scenarios = {10: "no_terminal", 11: "truncated", 12: "timeout", 13: "disconnect",
                 14: "native_error", 15: "failure", 16: "no_model", 17: "http_error"}
    session.mock.scenarios.update(scenarios)
    batch = session.submit(scenarios)
    items = {item["account_id"]: item for item in session.settle(batch["batch_id"])["items"]}
    for aid in (10, 11, 12, 13):
        summary(items[aid], actual=None if aid == 13 else MODEL, success=False, uncertain=True)
    for aid in (14, 15, 17):
        summary(items[aid], actual=None if aid == 17 else MODEL, success=False)
    summary(items[16], actual=None)
    require(sorted(session.mock.called()) == sorted(scenarios), "a failed/uncertain native request was replayed")
    session.privacy()
    return {"native_calls": len(scenarios), "uncertain": 4, "unsuccessful_terminal": 3, "missing_actual_model": None}


def contract_restart(session):
    ids = list(range(20, 25))
    session.mock.block(ids)
    body = session.payload(ids)
    batch = session.post(body).body
    bid = batch["batch_id"]
    wait_until(lambda: len(session.mock.called()) == 2, "restart fixture did not dispatch two requests")
    dispatched = set(session.mock.called())
    session.service.crash()
    session.mock.release()
    wait_until(lambda: not session.mock.active, "old fixture streams did not close after crash")
    session.service.start()
    final = session.settle(bid)
    items = {item["account_id"]: item for item in final["items"]}
    require(all(items[aid]["status"] == "needs_confirmation" for aid in dispatched), "running requests were not marked uncertain after restart")
    require(all(items[aid]["status"] == "completed" for aid in set(ids) - dispatched), "queued requests did not resume after restart")
    require(sorted(session.mock.called()) == ids, "restart replayed a previously dispatched request")
    require(session.post(body).body["batch_id"] == bid, "restart lost durable batch idempotency")
    cancelled = session.cancel(bid)
    require(all(item["status"] == "needs_confirmation" for item in cancelled["items"] if item["account_id"] in dispatched),
            "cancel hid uncertain work that had already reached native upstream")
    require({item["id"] for item in final["items"]} == {item["id"] for item in batch["items"]}, "restart changed durable item identities")
    session.privacy()
    return {"not_replayed": len(dispatched), "queued_resumed": len(ids) - len(dispatched), "batch_id_preserved": True}


def contract_auth_revalidation(session):
    session.mock.block([10, 11])
    batch = session.submit([10, 11, 12, 13])
    wait_until(lambda: len(session.mock.called()) == 2, "authorization fixture did not start")
    session.mock.authorized = False
    session.mock.release()
    # Fresh dispatch denial invalidates the route auth cache too. Restore the
    # fixture key only after both queued dispatches were denied, then inspect.
    wait_until(lambda: session.mock.auth_denied >= 2, "queued dispatches did not revalidate revoked authorization")
    session.mock.authorized = True
    final = session.settle(batch["batch_id"])
    items = {item["account_id"]: item for item in final["items"]}
    require(all(items[aid]["status"] == "failed" for aid in (12, 13)), "dispatch did not retain the explicit administrator rejection")
    require(sorted(session.mock.called()) == [10, 11] and session.mock.auth_denied >= 1, "revoked authorization did not prevent native dispatch")
    session.privacy()
    return {"native_calls": 2, "revoked_dispatches_blocked": 2}


def run_contract(args, database, temp_root):
    checks = [("health_auth_versions", contract_health, True), ("native_mapping_mixed_catalog_unavailable", contract_native, True),
        ("idempotency_conflicts", contract_idempotency, False), ("validation_1000_limit", contract_limits, False),
        ("cancel_undispatched", contract_cancel, False), ("parent_sibling_locks", contract_locks, True),
        ("dispatch_version_revalidation", contract_revalidation, False), ("native_failures_and_privacy", contract_failures, False),
        ("restart_without_replay", contract_restart, False), ("dispatch_auth_revalidation", contract_auth_revalidation, False)]
    results = []
    for name, function, special in checks:
        progress(f"contract {args.implementation}: {name}")
        started = time.monotonic()
        session = None
        try:
            with Session(args, database, temp_root, name, special=special,
                         stream_timeout=.4 if function is contract_failures else max(5, args.stream_timeout)) as session:
                detail = function(session)
            results.append({"name": name, "status": "passed", "seconds": round(time.monotonic() - started, 3), **detail})
        except Exception as exc:
            results.append({"name": name, "status": "failed", "error": safe_error(exc),
                            "seconds": round(time.monotonic() - started, 3)})
            if session is None:
                # A broken launch/schema is not ten independent HTTP failures.
                for remaining, _function, _special in checks[len(results):]:
                    results.append({"name": remaining, "status": "not_run", "reason": "fixture/service setup failed"})
                break
    return {"status": "passed" if all(row["status"] == "passed" for row in results) else "failed", "checks": results}


class CgroupSampler:
    def __init__(self, container, interval):
        pid = int(shell(["docker", "inspect", "--format", "{{.State.Pid}}", container]).stdout.strip())
        cgroups = Path(f"/proc/{pid}/cgroup").read_text().splitlines()
        relative = next((line[3:] for line in cgroups if line.startswith("0::")), None)
        require(relative is not None, "benchmark requires readable cgroup v2")
        self.path = Path("/sys/fs/cgroup") / relative.lstrip("/")
        require(self.path.is_relative_to("/sys/fs/cgroup"), "invalid container cgroup path")
        self.interval, self.samples, self.errors = interval, [], []
        self.lock, self.stop = threading.Lock(), threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.mark()
        self.thread.start()

    def mark(self):
        memory = int((self.path / "memory.current").read_text())
        cpu = dict(line.split() for line in (self.path / "cpu.stat").read_text().splitlines())
        sample = (time.monotonic(), memory, int(cpu["usage_usec"]))
        with self.lock:
            self.samples.append(sample)
        return sample[0]

    def loop(self):
        while not self.stop.wait(self.interval):
            try:
                self.mark()
            except Exception as exc:
                self.errors.append(type(exc).__name__)
                return

    def metrics(self, start, finish):
        require(not self.errors, "cgroup sampling stopped before measurement completed")
        with self.lock:
            values = sorted(sample for sample in self.samples if start <= sample[0] <= finish)
        require(len(values) >= 2, "resource measurement has fewer than two samples")
        elapsed = values[-1][0] - values[0][0]
        cpu_seconds = (values[-1][2] - values[0][2]) / 1_000_000
        peaks = [(b[2] - a[2]) / 10_000 / (b[0] - a[0]) for a, b in zip(values, values[1:]) if b[0] > a[0]]
        return {"samples": len(values), "seconds": round(elapsed, 4),
            "memory_start_bytes": values[0][1], "memory_mean_bytes": round(sum(row[1] for row in values) / len(values)),
            "memory_peak_bytes": max(row[1] for row in values), "cpu_seconds": round(cpu_seconds, 6),
            "cpu_mean_one_core_percent": round(100 * cpu_seconds / elapsed, 3),
            "cpu_peak_one_core_percent": round(max(peaks, default=0), 3)}

    def close(self):
        self.stop.set()
        self.thread.join(timeout=2)


def image_info(name):
    info = json.loads(shell(["docker", "image", "inspect", name]).stdout)[0]
    require(info["Os"] == "linux" and info["Architecture"] == "amd64", "benchmark image must be linux/amd64")
    return {"id": info["Id"], "os": info["Os"], "architecture": info["Architecture"], "full_size_bytes": info["Size"]}


def binary_size(args, session, temp_root):
    if args.implementation != "rust":
        return None
    path = args.binary_path
    if not path:
        path = json.loads(shell(["docker", "inspect", session.service.container]).stdout)[0]["Config"].get("Entrypoint")
        path = path[0] if path else None
    require(path and path.startswith("/"), "Rust benchmark requires --binary-path with its absolute path inside the image")
    target = temp_root / "measured-rust-binary"
    shell(["docker", "cp", session.service.container + ":" + path, str(target)])
    size = target.stat().st_size
    target.unlink()
    return size


def full_baseline(args, database, temp_root):
    database.full_schema()
    state = temp_root / "full-reference"
    state.mkdir(mode=0o700)
    result = {"status": "unmeasured", "scope": "full app.main empty fixture runtime idle only; no feature-equivalence or language-benefit claim",
        "production_comparison": "empty account/history/state data; not directly comparable to production memory",
        "image": image_info(args.full_image), "fixture_accounts": 0}
    with NativeMock({}) as mock:
        service = ProbeService(args, database, mock, state, full=True)
        sampler = None
        try:
            service.start()
            # app.main starts its real public fingerprint-bank sync in lifespan.
            # Do not label that initial loading work as idle or replace its logic.
            bank_path = state / "modeltrace-bank-state.json"
            def sync_finished():
                try:
                    bank = json.loads(bank_path.read_text())
                    return bank if bank.get("checked_at", 0) > 0 else None
                except (OSError, ValueError):
                    return None
            started = time.monotonic()
            try:
                bank = wait_until(sync_finished, "full runtime initial fingerprint sync did not settle within 65 seconds", timeout=65)
            except CheckFailure as exc:
                result["reason"] = str(exc)
                return result
            result["initial_public_sync"] = {"result": bank.get("result"),
                "error_category": (bank.get("last_error") or {}).get("code"),
                "seconds_waited": round(time.monotonic() - started, 3),
                "persisted_public_data_bytes": bank_path.stat().st_size}
            sampler = CgroupSampler(service.container, args.sample_interval)
            start = sampler.mark()
            time.sleep(args.idle_seconds)
            finish = sampler.mark()
            require(request(service.url, "/healthz", key=None).status == 200, "full reference healthz failed after idle window")
            result.update(status="measured", idle=sampler.metrics(start, finish))
            return result
        finally:
            if sampler:
                sampler.close()
            service.close()
            categories = {}
            audit = state / "audit.jsonl"
            if audit.exists():
                for line in audit.read_text().splitlines():
                    try:
                        row = json.loads(line)
                        event = str(row.get("event", "unknown"))
                        if not any(word in event for word in ("error", "failed", "failure")):
                            continue
                        if not re.fullmatch(r"[a-zA-Z0-9_:-]{1,100}", event):
                            event = "unknown_error_category"
                        categories[event] = categories.get(event, 0) + 1
                    except (ValueError, TypeError):
                        categories["unreadable_audit_record"] = categories.get("unreadable_audit_record", 0) + 1
            result["audit_error_categories"] = categories


def run_benchmark(args, database, temp_root):
    require(args.image, "benchmark requires --image to measure complete containers")
    result = {"status": "passed", "image": image_info(args.image), "concurrency": 2,
        "mock_delay_ms": args.delay_ms, "poll_interval_ms": 20, "resource_sample_interval_ms": args.sample_interval * 1000,
        "resource_method": "cgroup v2 memory.current and cpu.stat usage_usec; CPU percent uses one core as 100%",
        "latency_method": "wall HTTP request latency; linear-interpolated p50/p95; sample counts reported",
        "isolation": "fresh owned database rows and state per workload/repetition; host-network loopback fixture upstream",
        "container_uid": os.getuid(), "container_gid": os.getgid(),
        "workloads": [], "idle_samples": []}
    for count in (20, 100, 1000):
        post_ms, get_ms, elapsed_ms, runs = [], [], [], []
        for repetition in range(args.repetitions):
            name = f"benchmark-{count}-{repetition}"
            progress(f"benchmark {args.implementation}: {count} accounts, repetition {repetition + 1}/{args.repetitions}")
            with Session(args, database, temp_root, name, delay=args.delay_ms / 1000, stream_timeout=60) as session:
                sampler = CgroupSampler(session.service.container, args.sample_interval)
                try:
                    if count == 20:
                        start = sampler.mark()
                        time.sleep(args.idle_seconds)
                        finish = sampler.mark()
                        result["idle_samples"].append(sampler.metrics(start, finish))
                    if count == 20 and repetition == 0:
                        result["rust_binary_size_bytes"] = binary_size(args, session, temp_root)
                    if count == 1000:
                        session.mock.block(range(1, 1001))
                    body = session.payload(range(1, count + 1))
                    start = sampler.mark()
                    began = time.monotonic()
                    response = session.post(body)
                    post_ms.append(response.milliseconds)
                    if count == 1000:
                        finish = sampler.mark()
                        elapsed_ms.append((time.monotonic() - began) * 1000)
                        metrics = sampler.metrics(start, finish)
                        session.cancel(response.body["batch_id"])
                        session.mock.release()
                        session.settle(response.body["batch_id"], timeout=60)
                    else:
                        final = session.settle(response.body["batch_id"], latencies=get_ms, timeout=args.completion_timeout)
                        elapsed_ms.append((time.monotonic() - began) * 1000)
                        finish = sampler.mark()
                        metrics = sampler.metrics(start, finish)
                        require(all(item["status"] == "completed" and item["result"]["success"] is True for item in final["items"]),
                                "benchmark workload did not complete successfully")
                        require(len(session.mock.called()) == count and session.mock.max_active == 2,
                                "benchmark did not execute every fixture once at two concurrent native requests")
                    session.privacy()
                    runs.append({"repetition": repetition + 1, "elapsed_ms": round(elapsed_ms[-1], 3),
                                 "post_ms": round(post_ms[-1], 3), "resources": metrics})
                finally:
                    sampler.close()
        result["workloads"].append({"accounts": count, "scope": "acceptance_only_then_cancel" if count == 1000 else "all_completed",
            "post_latency_ms": distribution(post_ms), "get_latency_ms": distribution(get_ms),
            "elapsed_ms": distribution(elapsed_ms), "runs": runs})
    if args.full_image:
        # The third reference uses a separate empty database, never the slice's
        # populated probe fixture or the production database.
        try:
            with FixtureDatabase(database.admin_url) as reference:
                result["full_python_reference"] = full_baseline(args, reference, temp_root)
        except Exception as exc:
            result["full_python_reference"] = {"status": "failed", "error": safe_error(exc),
                "image": image_info(args.full_image), "scope": "full app.main empty fixture idle reference only"}
            result["status"] = "failed"
    return result


def json_argument(text, expected):
    try:
        value = json.loads(text)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("invalid JSON") from exc
    if expected is list:
        if not isinstance(value, list) or not value or not all(isinstance(part, str) and part for part in value):
            raise argparse.ArgumentTypeError("expected a nonempty JSON array of command arguments")
    elif not isinstance(value, dict) or not all(isinstance(key, str) and isinstance(item, str) for key, item in value.items()):
        raise argparse.ArgumentTypeError("expected a JSON object of string environment values")
    return value


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--mode", choices=("contract", "benchmark"), required=True)
    parser.add_argument("--implementation", choices=("python", "rust"), required=True)
    parser.add_argument("--command", type=lambda value: json_argument(value, list))
    parser.add_argument("--image")
    parser.add_argument("--database-url", default=os.getenv("PROBE_TEST_DATABASE_URL"))
    parser.add_argument("--postgres-image", default="postgres:16")
    parser.add_argument("--binary-path", help="absolute Rust executable path inside its image")
    parser.add_argument("--full-image", help="optional full app.main image; idle baseline only")
    parser.add_argument("--full-command", type=lambda value: json_argument(value, list))
    parser.add_argument("--full-env", default={}, type=lambda value: json_argument(value, dict),
        help="JSON env; values may use {database_url}, {state_dir}, {upstream}, {port}")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--delay-ms", type=float, default=50)
    parser.add_argument("--idle-seconds", type=float, default=2)
    parser.add_argument("--sample-interval", type=float, default=.05)
    parser.add_argument("--stream-timeout", type=float, default=5)
    parser.add_argument("--completion-timeout", type=float, default=180)
    args = parser.parse_args()
    require(not (args.image and args.command), "choose --command for a host process or --image for Docker")
    require(1 <= args.repetitions <= 30, "repetitions must be between 1 and 30")
    require(1 <= args.delay_ms <= 1000, "delay-ms must be between 1 and 1000")
    require(.2 <= args.idle_seconds <= 60, "idle-seconds must be between .2 and 60")
    require(.01 <= args.sample_interval <= 1, "sample-interval must be between .01 and 1")
    require(.1 <= args.stream_timeout <= 300, "stream-timeout must be between .1 and 300")
    require(1 <= args.completion_timeout <= 3600, "completion-timeout must be between 1 and 3600")
    require(not args.full_image or args.mode == "benchmark", "--full-image is only supported in benchmark mode")
    if args.image or args.full_image or args.mode == "benchmark":
        require(platform.system() == "Linux" and platform.machine() in {"x86_64", "amd64"}, "Docker comparison requires the same Linux x86_64 host")
        require(shutil.which("docker") is not None, "Docker is unavailable")
        require(json.loads(shell(["docker", "info", "--format", "{{json .}}"] ).stdout).get("OSType") == "linux", "Linux Docker engine required")
    return args


def safe_error(exc):
    if isinstance(exc, CheckFailure):
        return str(exc)
    if isinstance(exc, subprocess.TimeoutExpired):
        return "owned command exceeded its timeout"
    # Unexpected connector exceptions can contain credentials or local paths.
    return type(exc).__name__ + ": inspect the fixture service locally; no connection details emitted"


def main():
    result = {"fixture_only": True, "status": "failed"}
    try:
        args = parse_args()
        result.update(mode=args.mode, implementation=args.implementation, environment={
            "system": platform.system(), "architecture": platform.machine(), "python": platform.python_version(),
            "source_revision": shell(["git", "rev-parse", "HEAD"]).stdout.strip(),
            "source_has_local_changes": bool(shell(["git", "status", "--porcelain"]).stdout.strip())})
        with tempfile.TemporaryDirectory(prefix="sub2ops-probe-fixture-") as temporary:
            with FixtureDatabase(args.database_url, docker=bool(args.image), postgres_image=args.postgres_image) as database:
                result.update(run_contract(args, database, Path(temporary)) if args.mode == "contract"
                              else run_benchmark(args, database, Path(temporary)))
        result["cleanup"] = "complete"
    except KeyboardInterrupt:
        result.update(status="cancelled", error="interrupted")
    except Exception as exc:
        result.update(status="failed", error=safe_error(exc))
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
