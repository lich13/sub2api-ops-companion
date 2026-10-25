#!/usr/bin/env python3
"""Compare the complete fixed Python service and Rust service on one Linux host.

Only runner-owned PostgreSQL, state directories, containers and loopback mocks
are used. No production configuration or accounts are loaded.
"""
from __future__ import annotations

import argparse
from collections import Counter
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import tempfile
import threading
import time
import traceback
import uuid

import psycopg

from server_http_contract import (ADMIN_KEY, API, HttpClient, REFERENCE_SHA, ROOT,
    TemporaryPostgres, archive_reference_app, free_port, require, seed_fingerprint_cache)

BENCHMARK_FIELDS = frozenset({
    "Os", "Architecture", "Id", "Size", "Config", "Labels", "State", "ExitCode",
    "OOMKilled", "Error", "accounts", "id", "operation_versions", "test", "items",
    "batch_id", "pending", "status", "result", "success", "binary_bytes",
})


def docker(*arguments: str, include_stderr: bool = False) -> str:
    result = subprocess.run(["docker", *arguments], capture_output=True, text=True, check=False)
    require(result.returncode == 0, "Docker fixture command failed: " + arguments[0])
    return (result.stdout + (result.stderr if include_stderr else "")).strip()


def distribution(values: list[float]) -> dict:
    ordered = sorted(values)
    def percentile(q: float):
        if not ordered:
            return None
        index = (len(ordered) - 1) * q
        low = int(index)
        return round(ordered[low] + (ordered[min(low + 1, len(ordered) - 1)] - ordered[low]) * (index - low), 3)
    return {"samples": len(ordered), "p50": percentile(.5), "p95": percentile(.95)}


class Native:
    def __init__(self, delay: float):
        self.delay = delay
        self.lock = threading.Lock()
        self.hold = threading.Event()
        self.hold.set()
        self.calls: Counter[int] = Counter()
        self.active = self.peak = self.unexpected = 0
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, status: int, value: dict):
                raw = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                if self.path == "/api/v1/admin/groups/all":
                    authorized = self.headers.get("x-api-key") == ADMIN_KEY
                    self.reply(200 if authorized else 401, {"code": 0 if authorized else 401, "data": []})
                elif self.path == "/api/v1/admin/system/version":
                    self.reply(200, {"code": 0, "data": {"version": "0.2.9"}})
                else:
                    fixture.unexpected += 1
                    self.reply(404, {"code": 404})

            def do_POST(self):
                match = re.fullmatch(r"/api/v1/admin/accounts/(\d+)/test", self.path)
                if not match or self.headers.get("x-api-key") != ADMIN_KEY:
                    fixture.unexpected += 1
                    self.reply(404, {"code": 404})
                    return
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                account_id = int(match[1])
                with fixture.lock:
                    fixture.calls[account_id] += 1
                    fixture.active += 1
                    fixture.peak = max(fixture.peak, fixture.active)
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    self.wfile.write(("data: " + json.dumps({"type": "test_start", "model": body.get("model_id")}) + "\n\n").encode())
                    self.wfile.flush()
                    fixture.hold.wait(60)
                    time.sleep(fixture.delay)
                    self.wfile.write(b'data: {"type":"test_complete","success":true}\n\n')
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                finally:
                    with fixture.lock:
                        fixture.active -= 1

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": .05}, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.hold.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(3)


class Sampler:
    def __init__(self, container: str):
        pid = int(docker("inspect", "--format", "{{.State.Pid}}", container))
        group = next(line[3:] for line in Path(f"/proc/{pid}/cgroup").read_text().splitlines() if line.startswith("0::"))
        self.path = (Path("/sys/fs/cgroup") / group.lstrip("/")).resolve()
        require(self.path.is_relative_to("/sys/fs/cgroup"), "invalid cgroup path")
        self.samples = []
        self.errors = []
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.mark()
        self.thread.start()

    def mark(self):
        cpu = dict(line.split() for line in (self.path / "cpu.stat").read_text().splitlines())
        value = (time.monotonic(), int((self.path / "memory.current").read_text()), int(cpu["usage_usec"]))
        with self.lock:
            self.samples.append(value)
        return value[0]

    def loop(self):
        while not self.stop.wait(.05):
            try:
                self.mark()
            except Exception:
                self.errors.append("sampling_failed")
                return

    def metrics(self, start, finish):
        require(not self.errors, "resource sampling failed")
        with self.lock:
            rows = sorted(row for row in self.samples if start <= row[0] <= finish)
        require(len(rows) >= 2, "insufficient resource samples")
        elapsed = rows[-1][0] - rows[0][0]
        cpu = (rows[-1][2] - rows[0][2]) / 1_000_000
        return {"samples": len(rows), "seconds": round(elapsed, 4),
            "memory_mean_bytes": round(sum(row[1] for row in rows) / len(rows)),
            "memory_peak_bytes": max(row[1] for row in rows),
            "cpu_seconds": round(cpu, 6), "cpu_mean_one_core_percent": round(cpu / elapsed * 100, 3)}

    def close(self):
        self.stop.set()
        self.thread.join(3)


def failure_location(error: Exception) -> str:
    frames = traceback.extract_tb(error.__traceback__)
    script = Path(__file__).resolve()
    frame = next(
        (item for item in reversed(frames) if Path(item.filename).resolve() == script),
        None,
    )
    if frame is None:
        return type(error).__name__
    field = error.args[0] if isinstance(error, KeyError) and error.args else None
    field_hint = f" field={field}" if isinstance(field, str) and field in BENCHMARK_FIELDS else ""
    return f"{type(error).__name__} at {script.name}:{frame.lineno} ({frame.name}){field_hint}"


def shutdown_summary(implementation: str, state: dict, logs: str) -> dict:
    code = state.get("ExitCode")
    application_complete = "Application shutdown complete." in logs
    stopped = not state.get("Running", True) and not state.get("OOMKilled", False) and not state.get("Error")
    # Uvicorn 0.34.2 re-raises SIGTERM after lifespan shutdown. Docker reports
    # 128 + SIGTERM; its completion log distinguishes this from interruption.
    clean = stopped and (
        implementation == "rust" and code == 0
        or implementation == "python" and code in (0, 128 + signal.SIGTERM) and application_complete
    )
    return {"exit_code": code, "clean": bool(clean), "oom_killed": bool(state.get("OOMKilled")),
        "application_shutdown_complete": application_complete if implementation == "python" else None}


@contextmanager
def service(image: str, implementation: str, database: str, upstream: str, state: Path):
    name = "sub2ops-full-benchmark-" + uuid.uuid4().hex
    port = free_port()
    env = {"DATABASE_URL": database, "SUB2API_BASE_URL": upstream, "SUB2API_VERIFY_BASE_URL": "", "BASE_PATH": "",
        "HOST": "127.0.0.1", "PORT": str(port), "APP_PORT": str(port), "BARK_ENABLED": "false", "BARK_DEVICE_KEY": "",
        "OAUTH_RECOVERY_MONITOR_ENABLED": "false", "OAUTH_DAILY_TEST_ENABLED": "false", "OAUTH_AUTO_RESET_CREDIT_ENABLED": "false",
        "OAUTH_RECOVERY_TEST_CONCURRENCY": "2", "OAUTH_USAGE_REFRESH_CONCURRENCY": "2", "OAUTH_RECOVERY_TEST_MODEL_ID": "fixture-probe-model",
        "PYTHONDONTWRITEBYTECODE": "1", "PYTHONUNBUFFERED": "1"}
    for key, filename in {"AUDIT_PATH": "audit.jsonl", "USAGE_QUERY_STATE_PATH": "usage-query-state.json",
        "OAUTH_CONFIG_PATH": "oauth-config.json", "BARK_CONFIG_PATH": "bark-config.json",
        "SUB2API_CONFIG_PATH": "sub2api-config.json",
        "GROUP_MODEL_CONFIG_PATH": "group-model-config.json",
        "KEEPALIVE_CONFIG_PATH": "keepalive-state.json"}.items():
        env[key] = "/data/" + filename
    command = ["run", "--detach", "--init", "--name", name, "--network", "host", "--user", f"{os.getuid()}:{os.getgid()}",
        "--label", "sub2ops.fixture=full-runtime", "--mount", f"type=bind,src={state},dst=/data"]
    for key, value in env.items():
        command.extend(["--env", key + "=" + value])
    command.append(image)
    if implementation == "python":
        command.extend(["python", "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(port)])
    created = False
    shutdown = {}
    try:
        docker(*command)
        created = True
        client = HttpClient(f"http://127.0.0.1:{port}", timeout=30)
        deadline = time.monotonic() + 40
        while True:
            try:
                if client.request("GET", "/healthz").status == 200:
                    break
            except (OSError, TimeoutError):
                pass
            require(time.monotonic() < deadline, "full runtime did not become ready")
            time.sleep(.1)
        yield name, client, shutdown
    finally:
        if created:
            docker("stop", "--time", "60", name)
            info = json.loads(docker("inspect", name))[0]
            logs = docker("logs", name, include_stderr=True) if implementation == "python" else ""
            shutdown.update(shutdown_summary(implementation, info["State"], logs))
            docker("rm", name)
            require(shutdown["clean"], f"{implementation} runtime shutdown failed "
                f"(exit={shutdown['exit_code']}, oom={shutdown['oom_killed']}, "
                f"application_complete={shutdown['application_shutdown_complete']})")


def seed(database: str, count: int):
    with psycopg.connect(database, autocommit=True) as connection:
        connection.execute("TRUNCATE accounts CASCADE")
        with connection.cursor() as cursor:
            cursor.executemany("INSERT INTO accounts(id,name,platform,type,status,schedulable,credentials,extra,priority,concurrency,load_factor) "
                "VALUES (%s,%s,'openai','oauth','active',true,%s::jsonb,'{}'::jsonb,3,2,1)",
                [(number, f"fixture-benchmark-{number}", json.dumps({"access_token": "fixture-benchmark-token", "model_mapping": {}}))
                 for number in range(1, count + 1)])


def measure(args, image, implementation, reference, root, postgres):
    info = json.loads(docker("image", "inspect", image))[0]
    require(info["Os"] == "linux" and info["Architecture"] == "amd64", "benchmark requires Linux amd64 images")
    report = {"implementation": implementation, "image_id": info["Id"], "image_bytes": info["Size"],
        "build_revision": info.get("Config", {}).get("Labels", {}).get("org.opencontainers.image.revision"), "workloads": []}
    for count in (20, 100, 1000):
        posts, gets, elapsed, runs = [], [], [], []
        for iteration in range(args.repetitions):
            seed(postgres.url, count)
            state = root / f"{implementation}-{count}-{iteration}"
            state.mkdir(mode=0o700)
            seed_fingerprint_cache(reference, state)
            with Native(args.delay_ms / 1000) as native, service(image, implementation, postgres.url, native.url, state) as (container, client, shutdown):
                def api(method, path, body=None, expected=200):
                    begin = time.monotonic()
                    response = client.request(method, API + path, body=body, headers={"x-api-key": ADMIN_KEY})
                    milliseconds = (time.monotonic() - begin) * 1000
                    require(response.status == expected, f"benchmark API {method} {path.split('?')[0]} returned {response.status}")
                    return response.body, milliseconds
                snapshot, _ = api("GET", "/snapshot")
                rows = snapshot["accounts"]
                require(len(rows) == count, "full runtime account inventory differs")
                if implementation == "rust" and "binary_bytes" not in report:
                    binary = root / "measured-rust-binary"
                    docker("cp", container + ":/usr/local/bin/sub2ops-server", str(binary))
                    report["binary_bytes"] = binary.stat().st_size
                    report["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
                    binary.unlink()
                sampler = Sampler(container)
                try:
                    start = sampler.mark()
                    time.sleep(args.idle_seconds)
                    idle = sampler.metrics(start, sampler.mark())
                    if count == 1000:
                        native.hold.clear()
                    body = {"action": "test", "payload": {"confirmed": True}, "client_id": "fixture-full-benchmark",
                        "request_id": "fixture-" + uuid.uuid4().hex,
                        "accounts": [{"account_id": row["id"], "expected_version": row["operation_versions"]["test"]} for row in rows]}
                    start = sampler.mark()
                    accepted, post = api("POST", "/account-operations/batch", body, 202)
                    require(len(accepted["items"]) == count, "batch acceptance lost items")
                    posts.append(post)
                    if count == 1000:
                        finish = sampler.mark()
                        api("POST", "/account-operations/batches/" + accepted["batch_id"] + "/cancel", {})
                        native.hold.set()
                    deadline = time.monotonic() + 240
                    while True:
                        result, latency = api("GET", "/account-operations?batch_id=" + accepted["batch_id"])
                        gets.append(latency)
                        if result["pending"] == 0:
                            break
                        require(time.monotonic() < deadline, "batch did not settle")
                        time.sleep(.05)
                    if count != 1000:
                        finish = sampler.mark()
                        require(all(item["status"] == "completed" and item["result"]["success"] is True for item in result["items"]), "full batch failed")
                        require(len(native.calls) == count and all(n == 1 for n in native.calls.values()), "native requests missing or repeated")
                        require(native.peak == 2, "full runtime did not use two concurrent native requests")
                    require(native.unexpected == 0, "full runtime made unexpected upstream calls")
                    duration = (finish - start) * 1000
                    elapsed.append(duration)
                    runs.append({"repetition": iteration + 1, "elapsed_ms": round(duration, 3), "idle": idle,
                        "load": sampler.metrics(start, finish), "native_requests": sum(native.calls.values()),
                        "native_peak": native.peak, "shutdown": shutdown})
                finally:
                    native.hold.set()
                    sampler.close()
            audit = state / "audit.jsonl"
            if audit.exists():
                errors = [json.loads(line).get("event") for line in audit.read_text().splitlines() if line]
                require("runtime_task_error" not in errors, "Rust background cycle failed during benchmark")
        report["workloads"].append({"accounts": count, "scope": "acceptance_then_cancel" if count == 1000 else "complete_batch",
            "post_latency_ms": distribution(posts), "get_latency_ms": distribution(gets), "elapsed_ms": distribution(elapsed), "runs": runs})
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python-image", required=True)
    parser.add_argument("--rust-image", required=True)
    parser.add_argument("--postgres-bin-dir")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--delay-ms", type=float, default=50)
    parser.add_argument("--idle-seconds", type=float, default=2)
    args = parser.parse_args()
    require(platform.system() == "Linux" and platform.machine() in {"x86_64", "amd64"}, "run both full services on one Linux x86_64 host")
    require(1 <= args.repetitions <= 10 and 1 <= args.delay_ms <= 1000 and .2 <= args.idle_seconds <= 60, "invalid workload parameters")
    report = {"scope": "two complete Companion runtimes", "reference_sha": REFERENCE_SHA,
        "image_size_method": "docker image inspect Size on the same runner; storage-backend metadata, not host disk allocation",
        "resource_method": "cgroup v2 memory.current and cpu.stat at 50 ms; CPU 100% means one core",
        "latency_method": "HTTP wall time; linearly interpolated p50/p95", "concurrency": 2,
        "mock_delay_ms": args.delay_ms, "automatic_recovery": False, "daily_test": False, "auto_credit": False,
        "bark": False, "fingerprint_bank": "same cached reference bank", "runtimes": []}
    with tempfile.TemporaryDirectory(prefix="sub2ops-full-benchmark-") as directory:
        root = Path(directory)
        reference = archive_reference_app(root / "reference", REFERENCE_SHA)
        postgres = TemporaryPostgres(root, args.postgres_bin_dir)
        try:
            postgres.start()
            for implementation, image in [("python", args.python_image), ("rust", args.rust_image)]:
                report["runtimes"].append(measure(args, image, implementation, reference, root, postgres))
        finally:
            postgres.close()
    report["status"] = "passed"
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        # Exceptions from Docker/SQL must not emit credentials or host paths.
        from server_http_contract import CheckFailure
        diagnostic = str(error) if isinstance(error, CheckFailure) else failure_location(error)
        print(json.dumps({"status": "failed", "error": diagnostic}))
        raise SystemExit(1)
