#!/usr/bin/env python3
"""Run the complete Rust suite against an owned, disposable PostgreSQL cluster."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import platform
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

import psycopg
from psycopg import sql

from server_credit_contract import safe_environment
from server_http_contract import ROOT, SCHEMA_PATH, TemporaryPostgres


@contextmanager
def suite_database(postgres: TemporaryPostgres, index: int):
    """Give each test binary a fresh database inside this runner's owned cluster."""
    name = f"server_test_{index}"
    database_url = urlsplit(postgres.url)._replace(path=f"/{name}").geturl()
    with psycopg.connect(postgres.url, autocommit=True) as admin:
        admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
        try:
            with psycopg.connect(database_url, autocommit=True) as database:
                database.execute(SCHEMA_PATH.read_text(encoding="utf-8"))
            yield database_url
        finally:
            admin.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))


def run_cargo(command: list[str], database_url: str, scratch: Path) -> int:
    result = subprocess.run(command, cwd=ROOT, env=safe_environment(database_url, scratch),
                            capture_output=True, text=True)
    output = (result.stdout + result.stderr).replace(str(ROOT), "<repository>").replace(str(scratch), "<temporary>")
    output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
    print(output, end="", flush=True)
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-bin-dir")
    parser.add_argument("--test", help="Run one integration test binary")
    parser.add_argument("--lib", action="store_true", help="Run library tests only")
    parser.add_argument("--filter", help="Run matching tests in an ordinary integration binary")
    args = parser.parse_args()
    special = {
        "state_lifecycle_contract": "scripts/server_state_fixtures.py",
        "automation_runtime_contract": "scripts/server_automation_fixtures.py",
        "query_history_contract": "scripts/server_query_fixtures.py",
        "usage_records_parity": "scripts/server_usage_fixtures.py",
    }
    tests = {path.stem for path in (ROOT / "server" / "tests").glob("*.rs")}
    if args.lib:
        if args.test:
            parser.error("--lib cannot be combined with --test")
        tests = set()
    if args.test:
        if args.test not in tests:
            parser.error("unknown integration test binary")
        tests = {args.test}
    if args.filter and (not args.test and not args.lib or args.test in special):
        parser.error("--filter requires --lib or one ordinary integration test binary")
    ordinary = sorted(tests - special.keys())
    exit_code = 0
    # Darwin's default TMPDIR is long enough to overflow sockaddr_un after
    # adding the isolated scratch directory and a test socket filename.
    with tempfile.TemporaryDirectory(prefix="sub2ops-rust-tests-", dir="/tmp" if platform.system() == "Darwin" else None) as directory:
        scratch = Path(directory)
        postgres = TemporaryPostgres(scratch, args.postgres_bin_dir)
        try:
            postgres.start()
            command = ["cargo", "test", "--locked", "--offline", "--features", "test-fixtures", "--manifest-path", "server/Cargo.toml"]
            suites: list[list[str]] = []
            if args.lib:
                suites.append(["--lib"])
            elif not args.test:
                suites.append(["--lib", "--bins"])
            for test in ordinary:
                suites.append(["--test", test])
            if len(suites) > 1:
                # Compile all selected binaries together with Cargo's normal
                # build concurrency, then isolate their database mutations.
                selectors = [argument for suite in suites for argument in suite]
                exit_code = run_cargo(command + selectors + ["--no-run"], postgres.url, scratch)
            if not exit_code:
                for index, suite in enumerate(suites):
                    arguments = command + suite + ["--", "--test-threads=1"]
                    if args.filter:
                        arguments.append(args.filter)
                    with suite_database(postgres, index) as database_url:
                        exit_code = run_cargo(arguments, database_url, scratch)
                    if exit_code:
                        break
        finally:
            postgres.close()
    if exit_code:
        return exit_code
    # These suites also prepare state with the old writer and verify the Rust
    # output with the old reader; their runners own those extra fixtures.
    for test, runner in special.items():
        if test in tests:
            command = [sys.executable, runner]
            if args.postgres_bin_dir:
                command.extend(["--postgres-bin-dir", args.postgres_bin_dir])
            result = subprocess.run(command, cwd=ROOT, check=False)
            if result.returncode:
                return result.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
