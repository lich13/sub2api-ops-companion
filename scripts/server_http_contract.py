#!/usr/bin/env python3
"""Run fixture-only HTTP contracts against the Python reference or another service.

Examples (from the repository, using the project virtualenv):
  .venv/bin/python scripts/server_http_contract.py
  .venv/bin/python scripts/server_http_contract.py --command-json '["target/debug/sub2ops", "serve", "--host", "{host}", "--port", "{port}"]'
  .venv/bin/python scripts/server_http_contract.py --base-url http://127.0.0.1:18081

Reference and command modes start an owned service with a new loopback-only
PostgreSQL cluster, synthetic application rows, and local Sub2API, model
catalog, SSE, and Bark mocks. Reference mode extracts app/ from
6be7c291ed38dcc337cb88f693639040a3463899, so it remains usable after the
working-tree Python app is removed. Base-URL mode is deliberately read-only:
it sends no admin key and only probes health, unauthenticated route handling,
and aliases that reject missing client_version locally.

Only one JSON result is written to stdout. Progress goes to stderr; logs,
database files, state files, and child processes live in a runner-owned
temporary directory that is removed at exit.
"""
from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import uuid
from collections import Counter
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urljoin, urlsplit
from urllib.request import ProxyHandler, Request, build_opener


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_DIR = ROOT / "tests" / "contracts" / "http"
SCHEMA_PATH = CONTRACT_DIR / "schema.sql"
REFERENCE_SHA = "6be7c291ed38dcc337cb88f693639040a3463899"
API = "/api/desktop/v1"
ADMIN_KEY = "fixture-admin-server-contract-040"
WRONG_ADMIN_KEY = "fixture-invalid-server-contract-040"
CLIENT_KEY = "fixture-client-key-040"
CLIENT_KEY_NAME = "fixture-api-key-name-040"
CLIENT_KEY_TOKEN = "fixture-client-token-not-for-response"
ACCOUNT_TOKEN = "fixture-account-token-040"
USER_TOKEN = "fixture-user-token-not-for-response"
USAGE_TOKEN = "fixture-usage-token"
PRIVATE_BODY = "fixture-private-request-body"
PRIVATE_RESPONSE = "fixture-private-response-body"
PRIVATE_HEADER = "fixture-header-token"
PRIVATE_COOKIE = "fixture-cookie"
PRIVATE_ERROR = "fixture-error-token"
PRIVATE_ERROR_HEADER = "fixture-error-header"
PRIVATE_ERROR_BODY = "fixture-error-request-body"
PRIVATE_PASSWORD = "fixture-password-not-for-response"


class CheckFailure(RuntimeError):
    pass


def require(value: bool, message: str) -> None:
    if not value:
        raise CheckFailure(message)


def progress(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def safe_failure(exc: BaseException) -> str:
    if isinstance(exc, CheckFailure):
        return str(exc)
    if isinstance(exc, subprocess.TimeoutExpired):
        return "owned process exceeded its startup or shutdown timeout"
    if isinstance(exc, (URLError, TimeoutError, ConnectionError)):
        return "HTTP endpoint was unavailable"
    return type(exc).__name__ + "; inspect the fixture locally, no connection details emitted"


def command_result(command: list[str], *, timeout: float = 20, cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(command, cwd=cwd or ROOT, capture_output=True, timeout=timeout, check=False)


def archive_reference_app(destination: Path, sha: str) -> Path:
    require(bool(re.fullmatch(r"[0-9a-f]{40}", sha)), "reference SHA must be a full commit hash")
    destination.mkdir(mode=0o700, parents=True, exist_ok=True)
    archive_path = destination.parent / "reference-app.tar"
    with archive_path.open("wb") as archive_file:
        result = subprocess.run(
            ["git", "archive", "--format=tar", sha, "app"], cwd=ROOT,
            stdout=archive_file, stderr=subprocess.PIPE, timeout=60, check=False,
        )
    require(result.returncode == 0, "fixed Python reference could not be read from Git")
    try:
        with tarfile.open(archive_path, mode="r:") as archive:
            for member in archive:
                member_path = Path(member.name)
                require(not member_path.is_absolute() and ".." not in member_path.parts,
                        "reference archive contains an unsafe path")
                archive.extract(member, path=destination, filter="data")
    except (tarfile.TarError, OSError, TypeError) as exc:
        raise CheckFailure("fixed Python reference could not be extracted") from exc
    finally:
        archive_path.unlink(missing_ok=True)
    app_dir = destination / "app"
    require((app_dir / "main.py").is_file(), "fixed Python reference does not contain app.main")
    return app_dir


def seed_fingerprint_cache(app_dir: Path, state_dir: Path) -> None:
    """Keep the Python reference's hourly ModelTrace sync on bundled fixture data."""
    data_dir = app_dir / "modeltrace_data"
    manifest = json.loads((data_dir / "manifest.json").read_text(encoding="utf-8"))
    raw = (data_dir / "unified_bank.json").read_bytes()
    expected = manifest["files"]["bank"]["sha256"]
    require(hashlib.sha256(raw).hexdigest() == expected, "bundled model fixture checksum differs")
    built = json.loads(raw)["built_at"]
    now = time.time()
    version = {
        "revision": manifest["revision"],
        "sha256": expected,
        "built_at": built,
        "analyzer_version": manifest["analyzerVersion"],
        "core_blob": manifest["files"]["core"]["gitBlob"],
        "challenge_blob": manifest["files"]["challenge"]["gitBlob"],
    }
    state = {
        "version": 1,
        "active": {"raw_bank": raw.decode("utf-8"), "version": version},
        "checked_at": now,
        "synced_at": now,
        "source": "bundled",
        "cooldown_until": None,
        "last_error": None,
        "result": "fixture-cached",
    }
    (state_dir / "modeltrace-bank-state.json").write_text(
        json.dumps(state, ensure_ascii=False), encoding="utf-8"
    )


class TemporaryPostgres:
    def __init__(self, root: Path, bin_dir: str | None = None) -> None:
        self.root = root / "postgres"
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.data_dir = self.root / "data"
        self.log_path = self.root / "postgres.log"
        self.port = free_port()
        self.bin_dir = self._find_bin_dir(bin_dir)
        self.pg_ctl = str(self.bin_dir / "pg_ctl")
        self.initdb = str(self.bin_dir / "initdb")
        self.started = False
        self.url = ""
        self.postgres_pid: int | None = None

    @staticmethod
    def _find_bin_dir(explicit: str | None) -> Path:
        candidates = [Path(explicit)] if explicit else []
        for name in ("initdb", "pg_ctl"):
            found = shutil.which(name)
            if found:
                candidates.append(Path(found).parent)
        pg_config = shutil.which("pg_config")
        if pg_config:
            configured = subprocess.run([pg_config, "--bindir"], capture_output=True, text=True, check=False)
            if configured.returncode == 0:
                candidates.append(Path(configured.stdout.strip()))
        for prefix in ("/opt/homebrew/opt", "/usr/local/opt"):
            for version in ("17", "16", "15", "14"):
                candidates.append(Path(prefix) / f"postgresql@{version}" / "bin")
            candidates.append(Path(prefix) / "postgresql" / "bin")
        for candidate in candidates:
            if (candidate / "initdb").is_file() and (candidate / "pg_ctl").is_file():
                return candidate
        raise CheckFailure("temporary PostgreSQL binaries unavailable; provide --postgres-bin-dir")

    def start(self) -> None:
        init = command_result([
            self.initdb, "-D", str(self.data_dir), "-U", "contract_fixture",
            "--auth-local=trust", "--auth-host=trust", "--encoding=UTF8", "--no-instructions",
        ], timeout=60)
        require(init.returncode == 0, f"temporary PostgreSQL initdb exited {init.returncode}")
        # All fixture clients use TCP. Avoid the distribution's default Unix
        # socket directory, which may require a system PostgreSQL account.
        options = (
            f"-h 127.0.0.1 -p {self.port} -F -c listen_addresses=127.0.0.1 "
            "-c unix_socket_directories='' -c log_min_messages=warning -c timezone=UTC"
        )
        start = command_result([
            self.pg_ctl, "-D", str(self.data_dir), "-l", str(self.log_path),
            "-o", options, "-w", "-t", "30", "start",
        ], timeout=40)
        require(start.returncode == 0, f"temporary PostgreSQL pg_ctl exited {start.returncode}")
        self.started = True
        pid_path = self.data_dir / "postmaster.pid"
        try:
            self.postgres_pid = int(pid_path.read_text(encoding="ascii").splitlines()[0])
        except (OSError, ValueError, IndexError):
            self.postgres_pid = None
        try:
            import psycopg
        except ImportError as exc:
            raise CheckFailure("psycopg is required; use the project virtualenv") from exc
        admin = f"host=127.0.0.1 port={self.port} user=contract_fixture dbname=postgres sslmode=disable connect_timeout=2"
        with psycopg.connect(admin, autocommit=True) as connection:
            connection.execute("CREATE DATABASE server_http_contract")
        self.url = f"postgresql://contract_fixture@127.0.0.1:{self.port}/server_http_contract?sslmode=disable"
        with psycopg.connect(self.url, autocommit=True) as connection:
            connection.execute(SCHEMA_PATH.read_text(encoding="utf-8"))
            tables = connection.execute(
                "SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename"
            ).fetchall()
            names = {row[0] for row in tables}
            require({"accounts", "groups", "usage_logs", "ops_error_logs", "api_keys", "users"} <= names,
                    "synthetic application schema is incomplete")
            require("probe_accounts" not in names, "prototype probe_accounts table must not be used")

    def close(self) -> None:
        if not self.started:
            return
        stopped = command_result([
            self.pg_ctl, "-D", str(self.data_dir), "-m", "fast", "-w", "-t", "20", "stop",
        ], timeout=25)
        self.started = False
        require(stopped.returncode == 0, f"temporary PostgreSQL pg_ctl stop exited {stopped.returncode}")


class ExternalMocks:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.calls: Counter[str] = Counter()
        self.unclassified = 0
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args) -> None:
                pass

            def reply(self, status: int, value: object, content_type: str = "application/json") -> None:
                raw = value if isinstance(value, bytes) else json.dumps(value).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(raw)))
                self.send_header("Connection", "close")
                self.end_headers()
                try:
                    self.wfile.write(raw)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def count(self, category: str) -> None:
                with fixture.lock:
                    fixture.calls[category] += 1

            def do_GET(self) -> None:
                path = urlsplit(self.path).path
                if path == "/api/v1/admin/groups/all":
                    self.count("sub2api_auth")
                    valid = self.headers.get("x-api-key") == ADMIN_KEY
                    self.reply(200 if valid else 401, {"code": 0 if valid else 401, "data": []})
                    return
                if path in {"/v1/models", "/models", "/backend-api/codex/models"}:
                    self.count("model_directory")
                    self.reply(200, {"models": [{"id": "fixture-native-model", "slug": "fixture-native-model", "name": "Fixture Native Model"}]})
                    return
                if path == "/api/v1/admin/system/version":
                    self.count("sub2api_admin")
                    self.reply(200, {"code": 0, "data": {"version": "0.2.9"}})
                    return
                if path == "/api/v1/admin/proxies/all":
                    self.count("sub2api_admin")
                    self.reply(200, {"code": 0, "data": []})
                    return
                account_models = re.fullmatch(r"/api/v1/admin/accounts/\d+/models", path)
                if account_models:
                    self.count("sub2api_admin")
                    self.reply(200, {"code": 0, "data": [{"id": "fixture-native-model", "name": "Fixture Native Model"}]})
                    return
                with fixture.lock:
                    fixture.unclassified += 1
                self.reply(404, {"code": 404, "message": "fixture endpoint not found"})

            def do_POST(self) -> None:
                path = urlsplit(self.path).path
                length = int(self.headers.get("Content-Length", "0") or "0")
                raw = self.rfile.read(length) if length else b""
                if re.fullmatch(r"/api/v1/admin/accounts/\d+/test", path):
                    self.count("sse")
                    try:
                        payload = json.loads(raw or b"{}")
                    except ValueError:
                        payload = {}
                    requested = str(payload.get("model_id") or "")
                    actual = "fixture-native-model" if requested == "fixture-requested-model" else requested or "fixture-native-model"
                    events = [
                        {"type": "test_start", "model": actual},
                        {"type": "content", "text": "fixture-stream-content"},
                        {"type": "test_complete", "success": True},
                    ]
                    body = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
                    self.reply(200, body, "text/event-stream")
                    return
                if path.startswith("/push/") or path == "/push":
                    self.count("bark")
                    self.reply(200, {"code": "200"})
                    return
                if path.startswith("/api/v1/admin/"):
                    self.count("sub2api_admin")
                    self.reply(200, {"code": 0, "data": {}})
                    return
                with fixture.lock:
                    fixture.unclassified += 1
                self.reply(404, {"code": 404, "message": "fixture endpoint not found"})

            def do_PUT(self) -> None:
                self.do_POST()

            def do_DELETE(self) -> None:
                self.do_POST()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)

    def __enter__(self) -> "ExternalMocks":
        self.thread.start()
        return self

    def snapshot(self) -> dict[str, int]:
        with self.lock:
            return {
                **{name: self.calls[name] for name in ("sub2api_auth", "sub2api_admin", "model_directory", "sse", "bark")},
                "unclassified": self.unclassified,
            }

    def __exit__(self, *_args) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


@dataclass
class HttpResponse:
    status: int
    body: object
    content_type: str
    headers: dict[str, str] = field(default_factory=dict)


class HttpClient:
    def __init__(self, base_url: str, timeout: float = 8) -> None:
        self.base_url = base_url.rstrip("/") + "/"
        self.timeout = timeout
        self.opener = build_opener(ProxyHandler({}))

    def request(self, method: str, path: str, *, body: object | None = None,
                headers: dict[str, str] | None = None) -> HttpResponse:
        target = urljoin(self.base_url, path.lstrip("/"))
        request_headers = {"Accept": "application/json", **(headers or {})}
        data = None
        if body is not None:
            request_headers.setdefault("Content-Type", "application/json")
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        request = Request(target, data=data, headers=request_headers, method=method)
        try:
            response = self.opener.open(request, timeout=self.timeout)
        except HTTPError as exc:
            response = exc
        with response:
            raw = response.read(4_000_000)
            status = response.status
            content_type = response.headers.get("Content-Type", "")
            response_headers = {key.lower(): value for key, value in response.headers.items()}
        try:
            value = json.loads(raw) if raw else None
        except (UnicodeError, ValueError):
            value = {"non_json_response": True, "bytes": len(raw)}
        return HttpResponse(status, value, content_type, response_headers)


def url_is_loopback(base_url: str) -> bool:
    parsed = urlsplit(base_url)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        return False
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        return False
    try:
        return ipaddress.ip_address(parsed.hostname).is_loopback or parsed.hostname.lower() == "localhost"
    except ValueError:
        return False


def response_detail_shape(body: object) -> bool:
    return isinstance(body, dict) and (
        isinstance(body.get("detail"), (str, dict, list))
        or body.get("error") is not None
    )


def numeric_diagnostic(value: object) -> str:
    """Report only numeric values from synthetic HTTP fixtures on assertion failures."""
    if isinstance(value, bool):
        return "<non-numeric>"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str) and re.fullmatch(r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?", value):
        return value
    return "<non-numeric>"


@dataclass(frozen=True)
class RouteProbe:
    method: str
    path: str
    body: object | None = None
    allowed: tuple[int, ...] = (401,)


ROUTE_PROBES = [
    RouteProbe("GET", API + "/capabilities"),
    RouteProbe("GET", API + "/connection-status", allowed=(200, 401)),
    RouteProbe("GET", API + "/snapshot"),
    RouteProbe("GET", API + "/errors"),
    RouteProbe("GET", API + "/usage-records"),
    RouteProbe("GET", API + "/usage-record-options?kind=users"),
    RouteProbe("GET", API + "/usage-records/91001"),
    RouteProbe("GET", API + "/accounts/4001/quality"),
    RouteProbe("GET", API + "/errors/92001"),
    RouteProbe("POST", API + "/accounts/4001/operations", {} , (401, 422)),
    RouteProbe("GET", API + "/account-operations"),
    RouteProbe("POST", API + "/account-operations/batch", {}, (401, 422)),
    RouteProbe("POST", API + "/account-operations/batches/11111111111111111111111111111111/cancel"),
    RouteProbe("GET", API + "/account-operations/11111111111111111111111111111111"),
    RouteProbe("POST", API + "/account-operations/11111111111111111111111111111111/cancel"),
    RouteProbe("POST", API + "/accounts/4001/schedulable", {}, (401, 422)),
    RouteProbe("GET", API + "/recoveries"),
    RouteProbe("DELETE", API + "/accounts/4001", {}, (401, 422)),
    RouteProbe("POST", API + "/accounts/4001/recover-state", {}, (401, 422)),
    RouteProbe("POST", API + "/accounts/4001/priority", {}, (401, 422)),
    RouteProbe("PUT", API + "/accounts/4001/groups", {}, (401, 422)),
    RouteProbe("PUT", API + "/accounts/4001/degradation-mark", {}, (401, 422)),
    RouteProbe("GET", API + "/accounts/4001/settings"),
    RouteProbe("GET", API + "/proxies"),
    RouteProbe("POST", API + "/proxies/501/test"),
    RouteProbe("POST", API + "/proxies/501/quality-check"),
    RouteProbe("GET", API + "/accounts/4001/models"),
    RouteProbe("GET", API + "/account-templates"),
    RouteProbe("PUT", API + "/account-templates", {}, (401, 422)),
    RouteProbe("POST", API + "/account-templates", {}, (401, 422)),
    RouteProbe("POST", API + "/account-templates/apply", {}, (401, 422)),
    RouteProbe("POST", API + "/account-templates/custom", {}, (401, 422)),
    RouteProbe("PUT", API + "/account-templates/custom-aaaaaaaaaaaaaaaaaaaaaaaa", {}, (401, 422)),
    RouteProbe("PUT", API + "/account-templates/custom/custom-aaaaaaaaaaaaaaaaaaaaaaaa", {}, (401, 422)),
    RouteProbe("DELETE", API + "/account-templates/full", {}, (401, 422)),
    RouteProbe("DELETE", API + "/account-templates/custom/full", {}, (401, 422)),
    RouteProbe("GET", API + "/accounts/4001/model-detection"),
    RouteProbe("PUT", API + "/accounts/4001/model-detection", {}, (401, 422)),
    RouteProbe("GET", API + "/modeltrace/fingerprint-bank"),
    RouteProbe("POST", API + "/modeltrace/fingerprint-bank/sync"),
    RouteProbe("POST", API + "/accounts/4001/test", {}, (401, 422)),
    RouteProbe("POST", API + "/accounts/4001/model-tests", {}, (401, 422)),
    RouteProbe("GET", API + "/accounts/4001/model-tests/latest"),
    RouteProbe("GET", API + "/model-tests/11111111111111111111111111111111"),
    RouteProbe("POST", API + "/model-tests/11111111111111111111111111111111/cancel"),
    RouteProbe("POST", API + "/model-tests/11111111111111111111111111111111/retry", {}, (401, 422)),
    RouteProbe("POST", API + "/quota-refresh"),
    RouteProbe("GET", API + "/quota-refresh"),
    RouteProbe("GET", API + "/config"),
    RouteProbe("POST", API + "/accounts/4001/usage-action", {}, (401, 422)),
    RouteProbe("PUT", API + "/config/oauth", {}, (401, 422)),
    RouteProbe("POST", API + "/actions/bark-test"),
    RouteProbe("GET", API + "/model-groups"),
    RouteProbe("GET", API + "/model-groups/4301/reasoning"),
    RouteProbe("POST", API + "/model-groups/4301/reasoning/resolve", {}, (401, 422)),
    RouteProbe("PUT", API + "/model-groups/4301/reasoning", {}, (401, 422)),
    RouteProbe("DELETE", API + "/model-groups/4301/reasoning", {}, (401, 422)),
]


NOT_COVERED = [
    "manual_operation_queue_completion_and_idempotent_replay",
    "connection_test_batch_sse_completion_and_native_model_mapping",
    "queued_and_running_cancellation_boundaries",
    "truncated_or_disconnected_sse_unknown_outcome_recovery",
    "restart_reconciliation_without_replaying_dispatched_requests",
    "Bark_delivery_side_effect_and_retry_semantics",
]


class ContractSuite:
    def __init__(self, client: HttpClient, mocks: ExternalMocks | None = None) -> None:
        self.client = client
        self.mocks = mocks
        self.cases: list[dict[str, object]] = []
        self.request_ids: set[str] = set()

    def case(self, name: str, fn) -> None:
        try:
            evidence = fn() or {}
            self.cases.append({"name": name, "status": "passed", "evidence": evidence})
        except CheckFailure as exc:
            self.cases.append({"name": name, "status": "failed", "reason": str(exc)})
        except Exception as exc:
            self.cases.append({"name": name, "status": "failed", "reason": safe_failure(exc)})

    def expect_status(self, method: str, path: str, allowed: tuple[int, ...], *,
                      body: object | None = None, headers: dict[str, str] | None = None) -> HttpResponse:
        response = self.client.request(method, path, body=body, headers=headers)
        require(response.status in allowed,
                f"{method} {path.split('?')[0]} returned HTTP {response.status}; expected one of {allowed}")
        if path.startswith(API + "/"):
            request_id = response.headers.get("x-request-id", "")
            require(bool(re.fullmatch(r"[a-f0-9]{32}", request_id)),
                    "desktop response lacks its correlation ID")
            require(request_id not in self.request_ids, "desktop response reused its correlation ID")
            require(response.headers.get("cache-control") == "no-store",
                    "desktop response must prohibit caching")
            self.request_ids.add(request_id)
        return response

    @staticmethod
    def admin_headers() -> dict[str, str]:
        return {"x-api-key": ADMIN_KEY}

    def run_primary(self) -> None:
        self.case("healthz", self._health)
        self.case("admin_auth_required", self._auth_required)
        self.case("admin_auth_rejected", self._auth_rejected)
        self.case("admin_auth_accepted", self._auth_accepted)
        self.case("snapshot_shape_and_credential_redaction", self._snapshot)
        self.case("config_shape_and_secret_redaction", self._config)
        self.case("error_list_and_detail_redaction", self._errors)
        self.case("usage_query_and_detail_privacy", self._usage)
        self.case("usage_record_options_privacy", self._usage_options)
        self.case("parameter_and_response_error_semantics", self._errors_semantics)
        self.case("pinned_request_schema_rejections", self._request_schema_rejections)
        self.case("legacy_numeric_path_semantics", self._numeric_paths)
        self.case("model_catalog_route_aliases", self._model_aliases)
        self.case("unknown_route_semantics", self._unknown_route)
        self.case("retired_routes_remain_unavailable", self._retired_routes)
        self.case("all_declared_http_routes_resolve", self._route_inventory)
        self.case("external_calls_are_measured", self._external_calls)

    def _health(self) -> dict[str, object]:
        response = self.expect_status("GET", "/healthz", (200,))
        require(response.body == {"status": "ok"}, "healthz response shape differs")
        return {"status": response.status, "body_shape": "status=ok"}

    def _auth_required(self) -> dict[str, object]:
        response = self.expect_status("GET", API + "/snapshot", (401,))
        require(response_detail_shape(response.body), "missing-key response lacks a structured error")
        return {"status": response.status, "structured_error": True}

    def _auth_rejected(self) -> dict[str, object]:
        response = self.expect_status("GET", API + "/snapshot", (401,),
            headers={"x-api-key": WRONG_ADMIN_KEY})
        require(response_detail_shape(response.body), "rejected-key response lacks a structured error")
        serialized = json.dumps(response.body, ensure_ascii=False)
        require(WRONG_ADMIN_KEY not in serialized and ADMIN_KEY not in serialized,
                "authentication response echoed an API key")
        return {"status": response.status, "api_key_echoed": False}

    def _auth_accepted(self) -> dict[str, object]:
        response = self.expect_status("GET", API + "/capabilities", (200,), headers=self.admin_headers())
        require(isinstance(response.body, dict) and response.body.get("api_version") == 1,
                "capabilities response lacks api_version=1")
        require(response.body.get("service") == "Sub2API Ops Companion",
                "capabilities service identity differs")
        return {"status": response.status, "api_version": 1}

    def _snapshot(self) -> dict[str, object]:
        response = self.expect_status("GET", API + "/snapshot", (200,), headers=self.admin_headers())
        value = response.body
        require(isinstance(value, dict), "snapshot is not a JSON object")
        require({"schema_version", "observed_at", "accounts", "groups", "errors", "recoveries"} <= set(value),
                "snapshot is missing required top-level fields")
        accounts = value.get("accounts")
        require(isinstance(accounts, list) and any(row.get("id") == 4001 for row in accounts if isinstance(row, dict)),
                "snapshot did not read the synthetic accounts row")
        text = json.dumps(value, ensure_ascii=False)
        forbidden = (ACCOUNT_TOKEN, CLIENT_KEY, USER_TOKEN, USAGE_TOKEN, PRIVATE_PASSWORD, PRIVATE_BODY, PRIVATE_RESPONSE, PRIVATE_HEADER)
        require(not any(marker in text for marker in forbidden), "snapshot returned a fixture credential or private body")
        account = next(row for row in accounts if row.get("id") == 4001)
        require("credentials" not in account, "account DTO exposed credentials")
        return {"account_count": len(accounts), "synthetic_account_present": True, "private_markers_returned": 0}

    def _config(self) -> dict[str, object]:
        response = self.expect_status("GET", API + "/config", (200,), headers=self.admin_headers())
        value = response.body
        require(isinstance(value, dict) and {"oauth", "bark", "key_fallback"} <= set(value),
                "config response lacks a public section")
        for section in value.values():
            require(isinstance(section, dict) and bool(re.fullmatch(r"[a-f0-9]{64}", str(section.get("revision", "")))),
                    "config section revision is not a SHA-256 value")
        text = json.dumps(value, ensure_ascii=False)
        forbidden = (ADMIN_KEY, CLIENT_KEY, ACCOUNT_TOKEN, PRIVATE_PASSWORD, "fixture-bark-device-key-040")
        require(not any(marker in text for marker in forbidden), "config response exposed a secret value")
        return {"sections": sorted(value), "secret_markers_returned": 0}

    def _errors(self) -> dict[str, object]:
        headers = self.admin_headers()
        listing = self.expect_status("GET", API + "/errors?limit=20", (200,), headers=headers)
        require(isinstance(listing.body, dict) and isinstance(listing.body.get("items"), list),
                "error list does not contain items")
        require(any(item.get("id") == 92001 for item in listing.body["items"] if isinstance(item, dict)),
                "error list did not return the synthetic error row")
        detail = self.expect_status("GET", API + "/errors/92001", (200,), headers=headers)
        require(isinstance(detail.body, dict) and detail.body.get("id") == 92001,
                "error detail did not return the synthetic error row")
        text = json.dumps([listing.body, detail.body], ensure_ascii=False)
        forbidden = (PRIVATE_ERROR, PRIVATE_ERROR_HEADER, PRIVATE_ERROR_BODY, PRIVATE_BODY, PRIVATE_RESPONSE, ACCOUNT_TOKEN)
        require(not any(marker in text for marker in forbidden), "error response returned a private body or credential")
        require(isinstance(detail.body.get("content"), str), "error detail lacks safe content projection")
        return {"list_items": len(listing.body["items"]), "detail_content_present": True, "private_markers_returned": 0}

    def _usage(self) -> dict[str, object]:
        headers = self.admin_headers()
        listing = self.expect_status("GET", API + "/usage-records?include_summary=true", (200,), headers=headers)
        require(isinstance(listing.body, dict) and isinstance(listing.body.get("items"), list),
                "usage query does not contain items")
        require(any(item.get("id") == 91001 for item in listing.body["items"] if isinstance(item, dict)),
                "usage query did not return the synthetic usage row")
        require(isinstance(listing.body.get("summary"), dict), "usage query omitted requested summary")
        summary_actual_cost = listing.body["summary"].get("actual_cost")
        expected_actual_cost = "0.0250000000"
        require(summary_actual_cost == expected_actual_cost,
                f"usage summary actual_cost mismatch: expected {expected_actual_cost}, actual {numeric_diagnostic(summary_actual_cost)}")
        listed = next(item for item in listing.body["items"] if item.get("id") == 91001)
        require(listed.get("actual_cost") == "0.0250000000" and listed.get("request_type") == "sync",
                "usage row did not preserve its public cost and request classification")
        detail = self.expect_status("GET", API + "/usage-records/91001", (200,), headers=headers)
        require(isinstance(detail.body, dict) and detail.body.get("id") == 91001,
                "usage detail did not return the synthetic usage row")
        text = json.dumps([listing.body, detail.body], ensure_ascii=False)
        forbidden = (CLIENT_KEY, CLIENT_KEY_TOKEN, USER_TOKEN, USAGE_TOKEN, PRIVATE_BODY, PRIVATE_RESPONSE,
                     PRIVATE_HEADER, PRIVATE_COOKIE, PRIVATE_PASSWORD)
        require(not any(marker in text for marker in forbidden), "usage response returned credentials or request/response data")
        require("request_body" not in detail.body and "response_body" not in detail.body and "credentials" not in detail.body,
                "usage detail selected a non-public field")
        return {"usage_items": len(listing.body["items"]), "summary_present": True,
                "detail_id": 91001, "private_markers_returned": 0}

    def _usage_options(self) -> dict[str, object]:
        headers = self.admin_headers()
        users = self.expect_status("GET", API + "/usage-record-options?kind=users", (200,), headers=headers)
        keys = self.expect_status("GET", API + "/usage-record-options?kind=api_keys", (200,), headers=headers)
        require(isinstance(users.body, dict) and isinstance(users.body.get("items"), list),
                "user options do not contain items")
        require(isinstance(keys.body, dict) and isinstance(keys.body.get("items"), list),
                "API-key options do not contain items")
        user_text = json.dumps(users.body, ensure_ascii=False)
        key_text = json.dumps(keys.body, ensure_ascii=False)
        require("contract-040@example.invalid" in user_text and "fixture-user-040" in user_text,
                "user options did not return the synthetic user")
        require(CLIENT_KEY_NAME in key_text and CLIENT_KEY not in key_text and CLIENT_KEY_TOKEN not in key_text and
                USER_TOKEN not in user_text + key_text and PRIVATE_PASSWORD not in user_text + key_text,
                "usage filter options exposed a key, token, or password")
        return {"users": len(users.body["items"]), "api_keys": len(keys.body["items"]), "secret_markers_returned": 0}

    def _errors_semantics(self) -> dict[str, object]:
        headers = self.admin_headers()
        cases = [
            ("errors limit", "GET", API + "/errors?limit=0", None, 422, "detail"),
            ("errors category", "GET", API + "/errors?category=fixture-invalid", None, 422, "detail"),
            ("usage limit", "GET", API + "/usage-records?limit=0", None, 422, "detail"),
            ("usage time zone", "GET", API + "/usage-records?from_at=not-a-time", None, 422, "detail"),
            ("usage option kind", "GET", API + "/usage-record-options?kind=fixture-invalid", None, 422, "detail"),
            ("missing usage row", "GET", API + "/usage-records/999999", None, 404, "detail"),
            ("missing error row", "GET", API + "/errors/999999", None, 404, "detail"),
            ("invalid batch cursor", "GET", API + "/account-operations?batch_id=bad", None, 404, "detail"),
        ]
        for label, method, path, body, status, detail_key in cases:
            response = self.expect_status(method, path, (status,), body=body, headers=headers)
            require(isinstance(response.body, dict) and detail_key in response.body,
                    f"{label} returned an unstructured error")
        stale_config = {"expected_revision": "f" * 64, "changes": {"oauth_daily_test_enabled": False}}
        conflict = self.expect_status("PUT", API + "/config/oauth", (409,), body=stale_config, headers=headers)
        require(response_detail_shape(conflict.body), "stale config revision lacks a structured conflict")
        unknown_config = {"expected_revision": "f" * 64, "changes": {}}
        unknown = self.expect_status("PUT", API + "/config/fixture-invalid", (404,),
                                     body=unknown_config, headers=headers)
        require(response_detail_shape(unknown.body), "unknown config section lacks a structured 404")
        unknown_action = self.expect_status("POST", API + "/actions/fixture-invalid", (404,), headers=headers)
        require(response_detail_shape(unknown_action.body), "unknown action lacks a structured 404")
        recoveries = self.expect_status("GET", API + "/recoveries?limit=0", (422,), headers=headers)
        require(isinstance(recoveries.body, dict) and "detail" in recoveries.body,
                "invalid recovery pagination lacks a structured error")
        return {"validation_errors": 6, "missing_resources": 2, "unknown_batch": 404,
                "stale_config_revision": 409, "unknown_config_section": 404, "unknown_action": 404}

    def _model_aliases(self) -> dict[str, object]:
        headers = {"Authorization": "Bearer " + CLIENT_KEY}
        paths = ["/v1/models?client_version=fixture-040", "/models?client_version=fixture-040",
                 "/backend-api/codex/models"]
        for path in paths:
            response = self.expect_status("GET", path, (200,), headers=headers)
            require(isinstance(response.body, dict) and isinstance(response.body.get("models"), list),
                    f"model alias {path.split('?')[0]} lacks a models list")
            require(any(isinstance(model, dict) and model.get("id") == "fixture-native-model"
                        for model in response.body["models"]),
                    f"model alias {path.split('?')[0]} omitted the synthetic native model")
        return {"aliases": len(paths), "all_returned_models": True}

    def _request_schema_rejections(self) -> dict[str, object]:
        fixture = json.loads((ROOT / "tests/contracts/request-fixtures.json").read_text())
        require(fixture["reference_sha"] == REFERENCE_SHA, "request schema reference changed")
        cases = fixture["invalid_http_cases"]
        state_before = self.expect_status("GET", API + "/account-operations", (200,),
                                          headers=self.admin_headers()).body
        calls_before = self.mocks.snapshot() if self.mocks else None
        failures = []
        for case in cases:
            response = self.client.request(case["method"], API + case["path"],
                                           body=case["payload"], headers=self.admin_headers())
            if response.status != case["expected_status"]:
                failures.append({"case": case["id"], "status": response.status})
            if "fixture-private-input" in json.dumps(response.body):
                failures.append({"case": case["id"], "private_input_echoed": True})
        calls_after = self.mocks.snapshot() if self.mocks else None
        state_after = self.expect_status("GET", API + "/account-operations", (200,),
                                         headers=self.admin_headers()).body
        require(not failures, "request validation mismatches: " + json.dumps(failures, sort_keys=True))
        require(state_before == state_after, "invalid requests changed the operation state")
        require(calls_before == calls_after, "schema rejection must precede authentication and domain HTTP")
        return {"pinned_invalid_requests": len(cases), "private_input_echoed": False,
                "state_unchanged": True, "external_calls": 0 if calls_before is not None else "unmeasured"}

    def _unknown_route(self) -> dict[str, object]:
        response = self.expect_status("GET", API + "/fixture-unknown-route", (404,))
        require(response_detail_shape(response.body), "unknown route lacks a structured 404")
        config = {"expected_revision": "f" * 64, "changes": {}}
        unknown_section = self.expect_status("PUT", API + "/config/fixture-invalid", (404,), body=config)
        require(response_detail_shape(unknown_section.body), "unknown config section lacks a structured 404")
        no_client_version = self.expect_status("GET", "/models", (404,))
        require(response_detail_shape(no_client_version.body), "model alias without client_version lacks its 404")
        no_v1_client_version = self.expect_status("GET", "/v1/models", (404,))
        require(response_detail_shape(no_v1_client_version.body), "v1 model alias without client_version lacks its 404")
        return {"unknown_api_route": 404, "unknown_config_section": 404,
                "models_without_client_version": 404, "v1_models_without_client_version": 404}

    def _numeric_paths(self) -> dict[str, object]:
        paths = [
            ("/accounts/{id}/settings", 422),
            ("/accounts/{id}/quality", 422),
            ("/accounts/{id}/models", 404),
            ("/accounts/{id}/model-tests/latest", 200),
            ("/errors/{id}", 404),
            ("/usage-records/{id}", 422),
            ("/model-groups/{id}/reasoning", 422),
        ]
        for account_id in ("0", "-1", "0.0"):
            for path, status in paths:
                response = self.expect_status("GET", API + path.format(id=account_id), (status,),
                                              headers=self.admin_headers())
                if status == 200:
                    require(response.body is None, "an unknown account has no latest model test")
        return {"requests": len(paths) * 3, "domain_validation_preserved": True}

    def _retired_routes(self) -> dict[str, object]:
        paths = (
            "/", "/ops", "/sso", "/sso/start", "/oauth/settings", "/key-fallback/config",
            "/bark/config", "/bark/push-test", "/system/version", "/system/update",
            "/system/restart", "/logout", "/static/style.css", "/docs", "/redoc",
            "/openapi.json", "/telegram", "/telegram/config", "/telegram/push-test",
            "/telegram/oauth-settings", "/telegram/pairing-code/regenerate",
        )
        for path in paths:
            for method in ("GET", "POST"):
                self.expect_status(method, path, (404,))
        for action in ("telegram-pairing", "telegram-test"):
            self.expect_status("POST", API + "/actions/" + action, (404,))
        self.expect_status("PUT", API + "/config/telegram", (404,),
                           body={"changes": {}, "expected_revision": "a" * 64})
        return {"requests": 2 * len(paths) + 3, "authority_required": False}

    def _route_inventory(self) -> dict[str, object]:
        missing = []
        for probe in ROUTE_PROBES:
            response = self.client.request(probe.method, probe.path, body=probe.body)
            if response.status not in probe.allowed:
                missing.append({"method": probe.method, "path": probe.path.split("?")[0], "status": response.status})
        require(not missing, f"{len(missing)} declared routes did not resolve under unauthenticated probes")
        return {"declared_routes": len(ROUTE_PROBES), "missing_routes": 0}

    def _external_calls(self) -> dict[str, object]:
        require(self.mocks is not None, "command/reference mode has no external mock recorder")
        counts = self.mocks.snapshot()
        require(counts.get("sub2api_auth", 0) >= 1, "no Sub2API authentication call was recorded")
        require(counts.get("model_directory", 0) == 3, "expected exactly three model-directory alias requests")
        return {"total": sum(counts.values()), "by_category": counts}


def run_base_url(base_url: str, allow_nonloopback: bool) -> dict[str, object]:
    parsed = urlsplit(base_url)
    require(parsed.scheme in {"http", "https"} and parsed.hostname is not None,
            "base URL must be an absolute HTTP(S) URL")
    require(not parsed.username and not parsed.password and not parsed.query and not parsed.fragment,
            "base URL must not embed credentials, query parameters, or fragments")
    if not allow_nonloopback:
        require(url_is_loopback(base_url), "base URL must be loopback unless --allow-nonloopback-base-url is set")
    suite = ContractSuite(HttpClient(base_url), None)
    suite.case("healthz", suite._health)
    suite.case("admin_auth_required", suite._auth_required)
    suite.case("unknown_route_semantics", suite._unknown_route)
    suite.case("all_declared_http_routes_resolve", suite._route_inventory)
    failed = any(case["status"] == "failed" for case in suite.cases)
    return {
        "status": "failed" if failed else "partial",
        "base_url_probe_status": "failed" if failed else "passed",
        "cases": suite.cases,
        "not_covered": NOT_COVERED + [
            "synthetic_database_rows_and_authenticated_reads; base URL ownership is external",
            "external_call_counter; base URL target is not instrumented by the runner",
        ],
        "external_calls": {"measured": False, "reason": "external process is not instrumented"},
    }


class ServiceProcess:
    def __init__(self, root: Path, state_dir: Path, port: int, mocks: ExternalMocks,
                 database_url: str, *, command: list[str] | None, reference_app: Path | None,
                 target: str) -> None:
        self.root, self.state_dir, self.port, self.mocks = root, state_dir, port, mocks
        self.database_url, self.command, self.reference_app, self.target = database_url, command, reference_app, target
        self.process: subprocess.Popen | None = None
        self.log_path = root / "service.log"
        self.server_url = f"http://127.0.0.1:{port}"
        self.exit_code: int | None = None

    def _env(self) -> dict[str, str]:
        state = self.state_dir
        env = os.environ.copy()
        for name in tuple(env):
            upper = name.upper()
            if any(fragment in upper for fragment in (
                "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "API_KEY", "APIKEY",
                "PRIVATE_KEY", "PRIVATEKEY", "COOKIE", "ACCESS_KEY", "ACCESSKEY",
            )) or upper.startswith(("AWS_", "AZURE_", "GOOGLE_", "GITHUB_", "OPENAI_", "ANTHROPIC_", "SENTRY_")):
                env.pop(name, None)
        for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
            env.pop(name, None)
        isolated_tmp = state / "tmp"
        isolated_tmp.mkdir(mode=0o700, exist_ok=True)
        env.update({
            "TMPDIR": str(isolated_tmp),
            "TMP": str(isolated_tmp),
            "TEMP": str(isolated_tmp),
            "XDG_CONFIG_HOME": str(state / "xdg-config"),
            "XDG_CACHE_HOME": str(state / "xdg-cache"),
            "XDG_DATA_HOME": str(state / "xdg-data"),
            "NO_PROXY": "127.0.0.1,localhost,::1",
            "no_proxy": "127.0.0.1,localhost,::1",
            "DATABASE_URL": self.database_url,
            "SUB2API_BASE_URL": self.mocks.url,
            "SUB2API_VERIFY_BASE_URL": "",
            "SUB2API_CONFIG_PATH": str(state / "sub2api-config.json"),
            "BASE_PATH": "",
            "HOST": "127.0.0.1",
            "PORT": str(self.port),
            "APP_PORT": str(self.port),
            "APP_HOST": "127.0.0.1",
            "SERVER_HTTP_CONTRACT_HOST": "127.0.0.1",
            "SERVER_HTTP_CONTRACT_PORT": str(self.port),
            "USAGE_QUERY_STATE_PATH": str(state / "usage-query-state.json"),
            "AUDIT_PATH": str(state / "audit.jsonl"),
            "OAUTH_CONFIG_PATH": str(state / "oauth-config.json"),
            "BARK_CONFIG_PATH": str(state / "bark-config.json"),
            "KEY_FALLBACK_CONFIG_PATH": str(state / "key-fallback-config.json"),
            "GROUP_MODEL_CONFIG_PATH": str(state / "group-model-config.json"),
            "OAUTH_RECOVERY_MONITOR_ENABLED": "false",
            "OAUTH_DAILY_TEST_ENABLED": "false",
            "OAUTH_AUTO_RESET_CREDIT_ENABLED": "false",
            "OAUTH_USAGE_REFRESH_CONCURRENCY": "1",
            "OAUTH_RECOVERY_TEST_CONCURRENCY": "1",
            "OAUTH_DAILY_TEST_TIME": "23:59",
            "BARK_ENABLED": "false",
            "BARK_DEVICE_KEY": "fixture-bark-device-key-040",
            "BARK_SERVER_URL": self.mocks.url,
            "PYTHONUNBUFFERED": "1",
            "PYTHONDONTWRITEBYTECODE": "1",
        })
        (state / "bark-config.json").write_text(json.dumps({
            "enabled": False, "device_key": "fixture-bark-device-key-040", "server_url": self.mocks.url,
        }), encoding="utf-8")
        if self.reference_app is not None:
            python_root = self.reference_app.parent
            env["PYTHONPATH"] = str(python_root)
        else:
            env.pop("PYTHONPATH", None)
        return env

    def start(self) -> None:
        env = self._env()
        if self.command is None:
            require(self.reference_app is not None, "Python reference directory is missing")
            command = [sys.executable, "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(self.port)]
            cwd = self.reference_app.parent
        else:
            substitutions = {
                "{host}": "127.0.0.1",
                "{port}": str(self.port),
                "{base_url}": self.server_url,
                "{database_url}": self.database_url,
                "{state_dir}": str(self.state_dir),
                "{sub2api_url}": self.mocks.url,
            }
            command = []
            for token in self.command:
                for marker, value in substitutions.items():
                    token = token.replace(marker, value)
                command.append(token)
            cwd = ROOT
        with self.log_path.open("wb") as log:
            self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        client = HttpClient(self.server_url, timeout=0.5)

        def ready() -> bool:
            if self.process is not None and self.process.poll() is not None:
                # Only emit Rust's static phase labels, never arbitrary service logs.
                stages = re.findall(r"Sub2Ops startup stage [a-z_]+ failed \(\d+\)", self.log_path.read_text(errors="replace"))
                detail = "; ".join(stages)
                raise CheckFailure(f"service command exited {self.process.returncode} before healthz" + (f": {detail}" if detail else ""))
            try:
                return client.request("GET", "/healthz").status == 200
            except (URLError, TimeoutError, ConnectionError):
                return False

        deadline = time.monotonic() + 35
        while time.monotonic() < deadline:
            if ready():
                return
            time.sleep(0.05)
        raise CheckFailure("owned service did not become ready on loopback")

    def close(self) -> None:
        if self.process is None:
            return
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)
        self.exit_code = self.process.returncode
        self.process = None


def script_hash() -> str:
    digest = hashlib.sha256()
    for path in (Path(__file__), SCHEMA_PATH):
        raw = path.read_bytes()
        digest.update(path.name.encode("utf-8") + b"\0" + len(raw).to_bytes(8, "big") + raw)
    return digest.hexdigest()


def parse_command(value: str) -> list[str]:
    try:
        result = json.loads(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("command must be a JSON string array") from exc
    if not isinstance(result, list) or not result or not all(isinstance(item, str) and item for item in result):
        raise argparse.ArgumentTypeError("command must be a nonempty JSON string array")
    return result


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    target = parser.add_mutually_exclusive_group()
    target.add_argument("--command-json", type=parse_command,
                        help="JSON argv for a service command; tokens support {host}, {port}, {database_url}, {state_dir}, {sub2api_url}")
    target.add_argument("--base-url", help="read-only target URL; authenticated database tests require owned command mode")
    parser.add_argument("--postgres-bin-dir", help="directory containing initdb and pg_ctl")
    parser.add_argument("--allow-nonloopback-base-url", action="store_true",
                        help="allow unauthenticated read-only probes to a non-loopback base URL")
    return parser.parse_args()


def run_owned(args: argparse.Namespace) -> dict[str, object]:
    result: dict[str, object] = {
        "status": "failed",
        "primary_suite_status": "not_run",
        "target": "command" if args.command_json else "python-reference",
        "reference_sha": REFERENCE_SHA,
        "test_source_sha256": script_hash(),
        "not_covered": NOT_COVERED,
        "external_mock_endpoints": {"sub2api": True, "model_directory": True, "sse": True, "bark": True},
    }
    service: ServiceProcess | None = None
    postgres: TemporaryPostgres | None = None
    mocks: ExternalMocks | None = None
    temporary_path: Path | None = None
    suite: ContractSuite | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="server-http-contract-040-") as temporary:
            temporary_path = Path(temporary)
            state_dir = temporary_path / "state"
            state_dir.mkdir(mode=0o700)
            reference_root = temporary_path / "reference"
            reference_app = archive_reference_app(reference_root, REFERENCE_SHA)
            seed_fingerprint_cache(reference_app, state_dir)
            postgres = TemporaryPostgres(temporary_path, args.postgres_bin_dir)
            postgres.start()
            result["database"] = {
                "kind": "runner-owned-temporary-postgresql",
                "host": "127.0.0.1",
                "pid": postgres.postgres_pid,
                "application_tables": ["accounts", "groups", "account_groups", "users", "api_keys", "usage_logs", "ops_error_logs", "settings", "proxies", "composite_model_routes", "scheduled_test_plans"],
                "probe_accounts_used": False,
            }
            with ExternalMocks() as mocks:
                port = free_port()
                service = ServiceProcess(temporary_path, state_dir, port, mocks, postgres.url,
                    command=args.command_json, reference_app=None if args.command_json else reference_app,
                    target="command" if args.command_json else "python-reference")
                result["service"] = {"owned_by_runner": True, "host": "127.0.0.1", "pid": None}
                service.start()
                result["service"]["pid"] = service.process.pid if service.process else None
                suite = ContractSuite(HttpClient(service.server_url), mocks)
                suite.run_primary()
                result["cases"] = suite.cases
                failed = [case for case in suite.cases if case["status"] == "failed"]
                result["primary_suite_status"] = "failed" if failed else "passed"
                result["external_calls"] = {"measured": True, "by_category": mocks.snapshot(),
                    "total": sum(mocks.snapshot().values())}
                result["service_exit_code"] = None
                service.close()
                result["service_exit_code"] = service.exit_code
            postgres.close()
        result["temporary_directory_removed"] = temporary_path is not None and not temporary_path.exists()
        require(result["temporary_directory_removed"] is True, "runner-owned temporary directory was not removed")
        result["cleanup"] = {"service_process": "stopped", "postgres_process": "stopped", "temporary_files": "removed"}
        failed = any(case["status"] == "failed" for case in result.get("cases", []))
        result["status"] = "failed" if failed else "partial"
        return result
    except BaseException as exc:
        if service is not None:
            try:
                service.close()
            except Exception:
                pass
        if postgres is not None:
            try:
                postgres.close()
            except Exception:
                pass
        result["status"] = "failed"
        result["error"] = safe_failure(exc)
        result["service_exit_code"] = service.exit_code if service else None
        result["database_cleanup"] = "stopped" if postgres and not postgres.started else "failed_or_not_started"
        return result


def main() -> int:
    result: dict[str, object]
    try:
        args = parse_args()
        if args.base_url:
            result = run_base_url(args.base_url, args.allow_nonloopback_base_url)
            result["target"] = "base-url"
            result["test_source_sha256"] = script_hash()
        else:
            result = run_owned(args)
    except KeyboardInterrupt:
        result = {"status": "cancelled", "error": "interrupted", "test_source_sha256": script_hash()}
    except BaseException as exc:
        result = {"status": "failed", "error": safe_failure(exc), "test_source_sha256": script_hash()}
    result["exit_code"] = 0 if result.get("status") == "partial" or result.get("status") == "passed" else 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return int(result["exit_code"])


if __name__ == "__main__":
    raise SystemExit(main())
