#!/usr/bin/env python3
"""Run the complete Rust suite against an owned, disposable PostgreSQL cluster."""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from server_credit_contract import safe_environment
from server_http_contract import ROOT, TemporaryPostgres


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
    with tempfile.TemporaryDirectory(prefix="sub2ops-rust-tests-") as directory:
        scratch = Path(directory)
        postgres = TemporaryPostgres(scratch, args.postgres_bin_dir)
        try:
            postgres.start()
            command = ["cargo", "test", "--locked", "--offline", "--features", "test-fixtures", "--manifest-path", "server/Cargo.toml"]
            if args.lib:
                command.append("--lib")
            elif not args.test:
                command.extend(["--lib", "--bins"])
            for test in ordinary:
                command.extend(["--test", test])
            command.extend(["--", "--test-threads=1"])
            if args.filter:
                command.append(args.filter)
            if ordinary or not args.test:
                result = subprocess.run(command, cwd=ROOT, env=safe_environment(postgres.url, scratch),
                                        capture_output=True, text=True)
                output = (result.stdout + result.stderr).replace(str(ROOT), "<repository>").replace(directory, "<temporary>")
                output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
                print(output, end="")
                exit_code = result.returncode
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
