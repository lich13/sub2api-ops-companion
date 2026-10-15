#!/usr/bin/env python3
"""Exercise the CI Rust image, including supervised failure recovery.

The run owns its temporary PostgreSQL cluster, Compose copy, containers and
fixture mount. It never reads the repository's .env or data directory.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import re
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

from server_http_contract import CheckFailure, ROOT, TemporaryPostgres, require


SERVICE = "sub2api-ops-companion"
PORT = 18081


class SmokeFailure(RuntimeError):
    pass


def command(arguments: list[str], *, timeout: float = 30, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(arguments, capture_output=True, text=True, timeout=timeout, check=False, env=env)
    require(result.returncode == 0, "runtime smoke command failed: " + arguments[0])
    return result.stdout.strip()


def docker(*arguments: str, timeout: float = 30) -> str:
    return command(["docker", *arguments], timeout=timeout)


def docker_logs(name: str) -> str:
    result = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=15, check=False)
    require(result.returncode == 0, "runtime smoke could not read fixture logs")
    return result.stdout + result.stderr


def duration_ns(value: object) -> int:
    if isinstance(value, int):
        return value
    match = re.fullmatch(r"(\d+(?:\.\d+)?)(ns|us|ms|s|m|h)", str(value))
    require(match is not None, "Compose returned an invalid healthcheck duration")
    scales = {"ns": 1, "us": 1_000, "ms": 1_000_000, "s": 1_000_000_000,
              "m": 60_000_000_000, "h": 3_600_000_000_000}
    return int(float(match[1]) * scales[match[2]])


def compose_contract(image: str, scratch: Path) -> tuple[str, list[str], dict[str, int]]:
    source = ROOT / "docker-compose.yml"
    copied = scratch / "docker-compose.yml"
    shutil.copyfile(source, copied)
    (scratch / ".env").write_text("\n", encoding="utf-8")
    env = os.environ.copy()
    env.update({
        "OPS_IMAGE": image,
        "BUILD_REVISION": "runtime-smoke",
        "OPS_CONTAINER_USER": "10001:10001",
        "COMPOSE_PROJECT_NAME": "sub2ops-runtime-smoke",
    })
    rendered = command([
        "docker", "compose", "--project-directory", str(scratch), "--env-file", str(scratch / ".env"),
        "-f", str(copied), "config", "--format", "json",
    ], timeout=30, env=env)
    config = json.loads(rendered)
    service = config.get("services", {}).get(SERVICE, {})
    require(service.get("image") == image, "Compose did not select the requested CI image")
    require(service.get("user") == "10001:10001", "Compose container user is not the image's non-root user")
    restart = service.get("restart")
    require(restart == "unless-stopped", "Compose restart policy differs from unless-stopped")
    health = service.get("healthcheck") or {}
    test = health.get("test") or []
    require(len(test) >= 2 and test[0] == "CMD" and test[1] == "curl", "Compose healthcheck must invoke curl directly")
    require(test[-1] == f"http://127.0.0.1:{PORT}/healthz", "Compose healthcheck target differs")
    require("--max-time" in test and test[test.index("--max-time") + 1] == "2", "Compose curl timeout must be two seconds")
    interval = duration_ns(health.get("interval"))
    timeout = duration_ns(health.get("timeout"))
    start_period = duration_ns(health.get("start_period"))
    require(interval == 30_000_000_000, "Compose health interval must be 30 seconds")
    require(timeout == 3_000_000_000, "Compose health timeout must be three seconds")
    require(health.get("retries") == 3, "Compose health retry count must be three")
    require(start_period == 30_000_000_000, "Compose health start period must be 30 seconds")
    healthcheck = {
        "test": ["CMD-SHELL", " ".join(test[1:])],
        "interval": interval,
        "timeout": timeout,
        "retries": health["retries"],
        "start_period": start_period,
    }
    return restart, test[1:], healthcheck


def inspect_container(name: str) -> dict:
    return json.loads(docker("inspect", name))[0]


def create_args(name: str, restart: str, healthcheck: dict, env: dict[str, str], image: str,
                user: str, fixture: Path | None = None) -> list[str]:
    args = [
        "create", "--name", name, "--restart", restart, "--network", "host", "--user", user,
        "--label", "sub2ops.fixture=runtime-smoke",
        "--health-cmd", healthcheck["test"][1],
        "--health-interval", f"{healthcheck['interval'] / 1_000_000_000:g}s",
        "--health-timeout", f"{healthcheck['timeout'] / 1_000_000_000:g}s",
        "--health-retries", str(healthcheck["retries"]),
        "--health-start-period", f"{healthcheck['start_period'] / 1_000_000_000:g}s",
    ]
    if fixture is not None:
        args.extend(["--mount", f"type=bind,src={fixture},dst=/tmp/supervision_fixture,readonly", "--entrypoint", "/tmp/supervision_fixture"])
    for key, value in env.items():
        args.extend(["--env", f"{key}={value}"])
    args.append(image)
    if fixture is None:
        args.append("serve")
    return args


def service_environment(database_url: str) -> dict[str, str]:
    return {
        "DATABASE_URL": database_url,
        "HOST": "127.0.0.1",
        "PORT": str(PORT),
        "BASE_PATH": "",
        "BARK_ENABLED": "false",
        "BARK_DEVICE_KEY": "",
        "OAUTH_RECOVERY_MONITOR_ENABLED": "false",
        "OAUTH_DAILY_TEST_ENABLED": "false",
        "OAUTH_AUTO_RESET_CREDIT_ENABLED": "false",
        "OAUTH_RECOVERY_TEST_CONCURRENCY": "2",
        "OAUTH_USAGE_REFRESH_CONCURRENCY": "2",
        "AUDIT_PATH": "/data/audit.jsonl",
        "USAGE_QUERY_STATE_PATH": "/data/usage-query-state.json",
        "OAUTH_CONFIG_PATH": "/data/oauth-config.json",
        "BARK_CONFIG_PATH": "/data/bark-config.json",
        "KEY_FALLBACK_CONFIG_PATH": "/data/key-fallback-config.json",
        "SUB2API_CONFIG_PATH": "/data/sub2api-config.json",
        "GROUP_MODEL_CONFIG_PATH": "/data/group-model-config.json",
    }


def seed_modeltrace_cache(destination: Path) -> Path:
    resource_dir = ROOT / "server" / "resources" / "modeltrace"
    manifest = json.loads((resource_dir / "manifest.json").read_text(encoding="utf-8"))
    raw = (resource_dir / "unified_bank.json").read_bytes()
    require(hashlib.sha256(raw).hexdigest() == manifest["files"]["bank"]["sha256"],
            "bundled ModelTrace bank checksum differs")
    bank = json.loads(raw)
    version = {
        "revision": manifest["revision"],
        "sha256": manifest["files"]["bank"]["sha256"],
        "built_at": bank["built_at"],
        "analyzer_version": manifest["analyzerVersion"],
        "core_blob": manifest["files"]["core"]["gitBlob"],
        "challenge_blob": manifest["files"]["challenge"]["gitBlob"],
    }
    now = time.time()
    state = {
        "version": 1,
        "active": {"raw_bank": raw.decode("utf-8"), "version": version},
        "source": "bundled",
        "checked_at": now,
        "synced_at": now,
        "cooldown_until": None,
        "last_error": None,
        "result": "fixture-cached",
    }
    path = destination / "modeltrace-bank-state.json"
    path.write_text(json.dumps(state, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
    path.chmod(0o644)
    return path


def wait_healthy(name: str, timeout: float = 45) -> dict:
    deadline = time.monotonic() + timeout
    last_status = "starting"
    while time.monotonic() < deadline:
        info = inspect_container(name)
        state = info["State"]
        health = state.get("Health") or {}
        last_status = health.get("Status", last_status)
        if not state.get("Running"):
            raise SmokeFailure("runtime smoke container exited before becoming healthy")
        if last_status == "healthy":
            return info
        time.sleep(.25)
    raise SmokeFailure("runtime smoke container did not become healthy (status=" + last_status + ")")


def stop_remove(name: str) -> None:
    found = docker("ps", "-a", "--filter", "name=^/" + name + "$", "--format", "{{.Names}}")
    if found:
        subprocess.run(["docker", "stop", "--time", "5", name], capture_output=True, text=True, timeout=15, check=False)
        removed = subprocess.run(["docker", "rm", "-f", name], capture_output=True, text=True, timeout=15, check=False)
        require(removed.returncode == 0, "runtime smoke container cleanup failed")


def die_events(name: str) -> tuple[subprocess.Popen[str], queue.Queue[str]]:
    process = subprocess.Popen([
        "docker", "events", "--filter", "type=container", "--filter", f"container={name}",
        "--filter", "event=die", "--format", "{{json .Actor.Attributes}}",
    ], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
    events: queue.Queue[str] = queue.Queue()

    def read() -> None:
        assert process.stdout is not None
        for line in process.stdout:
            events.put(line)

    threading.Thread(target=read, daemon=True).start()
    return process, events


def first_exit_code(events: queue.Queue[str], timeout: float = 45) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            attributes = json.loads(events.get(timeout=min(.5, max(.01, deadline - time.monotonic()))))
        except queue.Empty:
            continue
        except json.JSONDecodeError as exc:
            raise SmokeFailure("Docker returned an invalid container event") from exc
        code = attributes.get("exitCode")
        if code is not None:
            return str(code)
    raise SmokeFailure("supervision fixture did not exit within the expected time")


def unexpected_fixture_exit(name: str, exit_code: str) -> SmokeFailure:
    logs = docker_logs(name)
    if "GLIBC_" in logs:
        return SmokeFailure("fixture binary requires a glibc version unavailable in the runtime image")
    if "No such file or directory" in logs:
        return SmokeFailure("fixture binary or its dynamic loader is unavailable in the runtime image")
    return SmokeFailure("supervised runtime fixture exited with unexpected status " + exit_code)


def verify_image(image: str) -> str:
    info = json.loads(docker("image", "inspect", image))[0]
    require(info.get("Os") == "linux" and info.get("Architecture") == "amd64", "runtime smoke requires Linux amd64")
    require(info.get("Config", {}).get("User") == "10001:10001", "formal image is not configured as a non-root user")
    version = docker("run", "--rm", "--entrypoint", "/usr/bin/curl", image, "--version")
    require(version.startswith("curl "), "formal image does not contain curl")
    return info["Config"]["User"]


def run(args: argparse.Namespace) -> None:
    require(platform.system() == "Linux" and platform.machine() in {"x86_64", "amd64"}, "runtime smoke requires a Linux amd64 runner")
    fixture = args.fixture.resolve(strict=True)
    require(fixture.is_file(), "supervision fixture executable is missing")
    user = verify_image(args.image)
    with tempfile.TemporaryDirectory(prefix="sub2ops-runtime-smoke-") as directory:
        scratch = Path(directory)
        restart, _, healthcheck = compose_contract(args.image, scratch)
        postgres = TemporaryPostgres(scratch, args.postgres_bin_dir)
        cache_seed = seed_modeltrace_cache(scratch)
        containers = []
        events_process = None
        try:
            postgres.start()
            env = service_environment(postgres.url)

            healthy_name = "sub2ops-runtime-healthy-" + uuid.uuid4().hex[:12]
            containers.append(healthy_name)
            docker(*create_args(healthy_name, restart, healthcheck, env, args.image, user))
            docker("cp", str(cache_seed), f"{healthy_name}:/data/modeltrace-bank-state.json")
            docker("start", healthy_name)
            info = wait_healthy(healthy_name)
            require(info["Config"].get("User") == user, "running service user differs from Compose")
            uid = docker("exec", healthy_name, "id", "-u")
            require(uid == "10001", "health service process is running as root")
            response = docker("exec", healthy_name, "/usr/bin/curl", "--fail", "--silent", "--show-error",
                              "--max-time", "2", f"http://127.0.0.1:{PORT}/healthz")
            try:
                health = json.loads(response)
            except json.JSONDecodeError as exc:
                raise SmokeFailure("health endpoint returned invalid JSON") from exc
            require(health.get("status") == "ok", "health endpoint status is not ok")
            stop_remove(healthy_name)
            containers.remove(healthy_name)

            failed_name = "sub2ops-runtime-failure-" + uuid.uuid4().hex[:12]
            containers.append(failed_name)
            docker(*create_args(failed_name, restart, healthcheck, env, args.image, user, fixture))
            docker("cp", str(cache_seed), f"{failed_name}:/data/modeltrace-bank-state.json")
            events_process, events = die_events(failed_name)
            docker("start", failed_name)
            exit_code = first_exit_code(events)
            if exit_code != "1":
                raise unexpected_fixture_exit(failed_name, exit_code)

            deadline = time.monotonic() + 45
            recovered = None
            while time.monotonic() < deadline:
                info = inspect_container(failed_name)
                state = info["State"]
                if info.get("RestartCount", 0) >= 1 and state.get("Running"):
                    recovered = wait_healthy(failed_name, max(.25, deadline - time.monotonic()))
                    break
                time.sleep(.25)
            require(recovered is not None, "unless-stopped did not restart the supervised runtime")
            require(recovered.get("RestartCount", 0) == 1, "supervised fixture restarted more than once")
            logs = docker_logs(failed_name)
            require("supervision_fixture_expected_failure" in logs, "fixture did not observe the serve failure")
            require("supervision_fixture_recovered" in logs, "restarted fixture did not reach recovery mode")
            uid = docker("exec", failed_name, "id", "-u")
            require(uid == "10001", "recovered service process is running as root")
            stop_remove(failed_name)
            containers.remove(failed_name)
        finally:
            try:
                for name in containers:
                    stop_remove(name)
            finally:
                if events_process is not None:
                    events_process.terminate()
                    try:
                        events_process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        events_process.kill()
                        events_process.wait(timeout=3)
                postgres.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--fixture", type=Path, required=True, help="test-fixture executable built with the test-fixtures feature")
    parser.add_argument("--postgres-bin-dir")
    args = parser.parse_args()
    try:
        run(args)
    except BaseException as exc:
        message = str(exc) if isinstance(exc, (CheckFailure, SmokeFailure, AssertionError)) else type(exc).__name__
        print(json.dumps({"status": "failed", "error": message}, ensure_ascii=False))
        return 1
    print(json.dumps({
        "status": "passed",
        "checks": ["non-root image", "curl healthcheck", "/healthz", "supervised nonzero exit", "unless-stopped recovery"],
        "not_covered": ["production environment and data", "cross-platform runtime behavior"],
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
