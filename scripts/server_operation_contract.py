#!/usr/bin/env python3
"""Run the Rust operation contract against temporary app-schema PostgreSQL.

Run from the repository with the project Python environment:
  .venv/bin/python scripts/server_operation_contract.py

The runner reuses the temporary PostgreSQL lifecycle from
server_http_contract.py, then invokes only the Rust operation integration
target. Its Axum upstream mock is started and stopped by the Rust test process.
The database, PostgreSQL logs, and Rust temporary state are confined to
runner-owned temporary locations; no external service is contacted.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
TEST_PATH = ROOT / "server" / "tests" / "operation_contract.rs"
FIXTURE_PATH = ROOT / "tests" / "contracts" / "operation-fixtures.json"
IMPLEMENTATION_PATHS = (
    "server/src/actions.rs",
    "server/src/operations.rs",
    "server/src/native_batches.rs",
    "server/src/account_locks.rs",
    "server/src/account_templates.rs",
    "server/src/validation.rs",
    "server/src/store.rs",
    "server/src/db.rs",
    "server/src/error.rs",
    "server/src/upstream.rs",
    "server/src/queries.rs",
    "server/src/sql.rs",
    "server/src/compat.rs",
)

# Importing the existing runner avoids duplicating its initdb/pg_ctl and schema
# setup. Keep Python from leaving __pycache__ outside this runner's temp area.
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts"))
try:
    from server_http_contract import TemporaryPostgres
except ImportError as exc:  # pragma: no cover - diagnosed in the result object
    TemporaryPostgres = None  # type: ignore[assignment,misc]
    IMPORT_ERROR = type(exc).__name__
else:
    IMPORT_ERROR = ""


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def implementation_hash() -> str:
    digest = hashlib.sha256()
    for relative in IMPLEMENTATION_PATHS:
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update((ROOT / relative).read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def safe_environment(database_url: str, temporary: Path) -> dict[str, str]:
    env = os.environ.copy()
    secret_markers = (
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "API_KEY",
        "APIKEY",
        "PRIVATE_KEY",
        "PRIVATEKEY",
        "COOKIE",
        "ACCESS_KEY",
        "ACCESSKEY",
    )
    for name in tuple(env):
        upper = name.upper()
        if any(marker in upper for marker in secret_markers) or upper.startswith(
            (
                "AWS_",
                "AZURE_",
                "GITHUB_",
                "OPENAI_",
                "ANTHROPIC_",
                "BARK_",
                "OAUTH_",
                "SUB2API_",
                "SUB2OPS_",
            )
        ):
            env.pop(name, None)
    for name in (
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ):
        env.pop(name, None)
    scratch = temporary / "scratch"
    scratch.mkdir(mode=0o700, exist_ok=True)
    env.update(
        {
            "DATABASE_URL": database_url,
            "KEEPALIVE_CONFIG_PATH": str(temporary / "keepalive-state.json"),
            "TMPDIR": str(scratch),
            "TMP": str(scratch),
            "TEMP": str(scratch),
            "NO_PROXY": "127.0.0.1,localhost,::1",
            "no_proxy": "127.0.0.1,localhost,::1",
            "CARGO_NET_OFFLINE": "true",
            "RUST_TEST_THREADS": "1",
            "SUB2OPS_OPERATION_CONTRACT": "1",
        }
    )
    return env


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--postgres-bin-dir",
        help="directory containing initdb and pg_ctl when they are not on PATH",
    )
    return parser.parse_args()


def scrub(text: str, temporary: Path, database_url: str) -> str:
    text = text.replace(str(ROOT), "<repository>")
    text = text.replace(str(temporary), "<runner-temp>")
    text = text.replace(database_url, "<temporary-database-url>")
    text = re.sub(
        r"(?i)(authorization|x-api-key|database_url|password)\s*[:=]\s*[^\s,]+",
        r"\1=<redacted>",
        text,
    )
    return text


def cargo_summary(output: str) -> dict[str, Any]:
    lines = re.findall(r"^\s*test ([A-Za-z0-9_:]+) \.\.\. (ok|FAILED)$", output, re.M)
    summaries = re.findall(
        r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; "
        r"(\d+) ignored; (\d+) measured; (\d+) filtered out",
        output,
    )
    summary: dict[str, Any] = {
        "passed_tests": [name for name, status in lines if status == "ok"],
        "failed_tests": [name for name, status in lines if status == "FAILED"],
    }
    if summaries:
        status, passed, failed, ignored, measured, filtered = summaries[-1]
        summary["test_result"] = {
            "status": "passed" if status == "ok" else "failed",
            "passed": int(passed),
            "failed": int(failed),
            "ignored": int(ignored),
            "measured": int(measured),
            "filtered_out": int(filtered),
        }
    else:
        summary["test_result"] = None
    return summary


def cargo_diagnostics(output: str) -> list[str]:
    selected = []
    for line in output.splitlines():
        line = re.sub(r"\x1b\[[0-9;]*m", "", line)
        if re.match(r"^\s*error(?:\[E\d+\])?: ", line):
            selected.append(line.strip())
        elif re.match(r"^\s*test [A-Za-z0-9_:]+ \.\.\. FAILED$", line):
            selected.append(line.strip())
        elif "panicked at" in line:
            location = re.search(r"([^ ]+\.rs:\d+:\d+)", line)
            if location:
                selected.append("panic at " + location.group(1))
            else:
                selected.append("test panic")
        elif re.match(r"^\s*assertion .{1,160} failed$", line):
            selected.append(line.strip())
        elif re.match(r"^\s*(?:left|right): ", line):
            selected.append(line.strip()[:240])
        elif "Result::unwrap()" in line and "Err" in line:
            selected.append(line.strip()[:240])
    return selected[:30]


def run_owned(args: argparse.Namespace) -> dict[str, Any]:
    result: dict[str, Any] = {
        "status": "failed",
        "reference_sha": REFERENCE_SHA,
        "command": (
            "cargo test --manifest-path server/Cargo.toml --locked --offline "
            "--test operation_contract -- --test-threads=1 --nocapture"
        ),
        "postgres": {
            "kind": "runner-owned temporary PostgreSQL",
            "loopback_only": True,
            "schema": "tests/contracts/http/schema.sql",
            "application_tables_only": True,
        },
        "upstream_mock": "Rust Axum loopback fixture",
        "source_sha256": {
            "operation_contract.rs": file_sha256(TEST_PATH) if TEST_PATH.is_file() else None,
            "operation-fixtures.json": file_sha256(FIXTURE_PATH) if FIXTURE_PATH.is_file() else None,
            "schema.sql": file_sha256(ROOT / "tests" / "contracts" / "http" / "schema.sql")
            if (ROOT / "tests" / "contracts" / "http" / "schema.sql").is_file()
            else None,
            "Cargo.toml": file_sha256(ROOT / "server" / "Cargo.toml")
            if (ROOT / "server" / "Cargo.toml").is_file()
            else None,
            "Cargo.lock": file_sha256(ROOT / "server" / "Cargo.lock")
            if (ROOT / "server" / "Cargo.lock").is_file()
            else None,
            "runner": file_sha256(Path(__file__).resolve()),
            "implementation": implementation_hash(),
        },
        "implementation_sha256_after": None,
        "exit_code": None,
    }
    if IMPORT_ERROR:
        result["error"] = "existing temporary PostgreSQL runner unavailable: " + IMPORT_ERROR
        return result
    if not TEST_PATH.is_file() or not FIXTURE_PATH.is_file():
        result["error"] = "operation test source or fixture is missing"
        return result
    if not (ROOT / "tests" / "contracts" / "http" / "schema.sql").is_file():
        result["error"] = "temporary app schema fixture is missing"
        return result
    head = subprocess.run(
        ["git", "rev-parse", "--verify", REFERENCE_SHA + "^{commit}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if head.returncode != 0 or head.stdout.strip() != REFERENCE_SHA:
        result["error"] = "fixed Python reference commit is unavailable; fetch repository history"
        return result
    if TemporaryPostgres is None:
        result["error"] = "temporary PostgreSQL lifecycle helper unavailable"
        return result

    temporary_path: Path | None = None
    postgres = None
    cleanup_error = ""
    try:
        with tempfile.TemporaryDirectory(prefix="server-operation-contract-040-") as name:
            temporary_path = Path(name)
            postgres = TemporaryPostgres(temporary_path, args.postgres_bin_dir)
            postgres.start()
            result["postgres"]["started"] = True
            source_before = implementation_hash()
            result["source_sha256"]["implementation"] = source_before
            env = safe_environment(postgres.url, temporary_path)
            command = [
                "cargo",
                "test",
                "--manifest-path",
                "server/Cargo.toml",
                "--locked",
                "--offline",
                "--test",
                "operation_contract",
                "--",
                "--test-threads=1",
                "--nocapture",
            ]
            completed = subprocess.run(
                command,
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            output = (completed.stdout or "") + "\n" + (completed.stderr or "")
            result["exit_code"] = completed.returncode
            result["tests"] = cargo_summary(output)
            result["diagnostics"] = [
                scrub(line, temporary_path, postgres.url)
                for line in cargo_diagnostics(output)
            ]
            result["implementation_sha256_after"] = implementation_hash()
            if result["implementation_sha256_after"] != source_before:
                result["error"] = "implementation sources changed while the contract was running"
            result["postgres"]["stopped"] = False
            try:
                postgres.close()
                result["postgres"]["stopped"] = True
            except BaseException as exc:
                cleanup_error = type(exc).__name__
    except BaseException as exc:
        result["error"] = "runner failed: " + type(exc).__name__
        if postgres is not None and getattr(postgres, "started", False):
            try:
                postgres.close()
                result["postgres"]["stopped"] = True
            except BaseException as cleanup_exc:
                cleanup_error = type(cleanup_exc).__name__

    result["temporary_directory_removed"] = (
        temporary_path is None or not temporary_path.exists()
    )
    if cleanup_error:
        result["cleanup_error"] = cleanup_error
    test_result = result.get("tests", {}).get("test_result")
    passed = (
        result.get("exit_code") == 0
        and test_result is not None
        and test_result.get("status") == "passed"
        and test_result.get("passed", 0) > 0
        and result.get("implementation_sha256_after")
        == result["source_sha256"]["implementation"]
        and result.get("postgres", {}).get("stopped") is True
        and result.get("temporary_directory_removed") is True
        and not cleanup_error
    )
    result["status"] = "passed" if passed else "failed"
    if not passed and result.get("exit_code") == 0:
        result["exit_code"] = 1
    return result


def main() -> int:
    try:
        args = parse_args()
        result = run_owned(args)
    except KeyboardInterrupt:
        result = {"status": "cancelled", "error": "interrupted", "exit_code": 130}
    except BaseException as exc:
        result = {"status": "failed", "error": type(exc).__name__, "exit_code": 1}
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return int(result.get("exit_code") if result.get("exit_code") is not None else 1)


if __name__ == "__main__":
    raise SystemExit(main())
