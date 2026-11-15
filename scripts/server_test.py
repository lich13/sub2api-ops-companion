#!/usr/bin/env python3
"""Run the complete Rust suite against an owned, disposable PostgreSQL cluster."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import os
import platform
import re
import subprocess
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

import psycopg
from psycopg import sql

from server_http_contract import ROOT, SCHEMA_PATH, TemporaryPostgres

# These suites assert on the SQL their read paths execute; each reads the
# cluster's statement log through the named variable.
STATEMENT_LOGS = {
    "query_history_contract": "SERVER_QUERY_HISTORY_PG_LOG",
    "usage_records_parity": "SERVER_USAGE_PARITY_PG_LOG",
}


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
                "PG",
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
        }
    )
    return env


@contextmanager
def suite_database(postgres: TemporaryPostgres, index: int, log_statements: bool):
    """Give each test binary a fresh database inside this runner's owned cluster."""
    name = f"server_test_{index}"
    database_url = urlsplit(postgres.url)._replace(path=f"/{name}").geturl()
    with psycopg.connect(postgres.url, autocommit=True) as admin:
        admin.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
        try:
            with psycopg.connect(database_url, autocommit=True) as database:
                database.execute(SCHEMA_PATH.read_text(encoding="utf-8"))
            if log_statements:
                admin.execute(sql.SQL("ALTER DATABASE {} SET log_statement = 'all'").format(sql.Identifier(name)))
            yield database_url
        finally:
            admin.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))


def run_cargo(command: list[str], database_url: str, scratch: Path, extra: dict[str, str] | None = None) -> int:
    env = safe_environment(database_url, scratch)
    env.update(extra or {})
    result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    output = (result.stdout + result.stderr).replace(str(ROOT), "<repository>").replace(str(scratch), "<temporary>")
    output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
    print(output, end="", flush=True)
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-bin-dir")
    parser.add_argument("--test", help="Run one integration test binary")
    parser.add_argument("--lib", action="store_true", help="Run library tests only")
    parser.add_argument("--filter", help="Run matching tests in the selected binary")
    args = parser.parse_args()
    tests = {path.stem for path in (ROOT / "server" / "tests").glob("*.rs")}
    if args.lib:
        if args.test:
            parser.error("--lib cannot be combined with --test")
        tests = set()
    if args.test:
        if args.test not in tests:
            parser.error("unknown integration test binary")
        tests = {args.test}
    if args.filter and not args.test and not args.lib:
        parser.error("--filter requires --lib or one integration test binary")
    exit_code = 0
    # Darwin's default TMPDIR is long enough to overflow sockaddr_un after
    # adding the isolated scratch directory and a test socket filename.
    with tempfile.TemporaryDirectory(prefix="sub2ops-rust-tests-", dir="/tmp" if platform.system() == "Darwin" else None) as directory:
        scratch = Path(directory)
        postgres = TemporaryPostgres(scratch, args.postgres_bin_dir)
        try:
            postgres.start()
            with psycopg.connect(postgres.url, autocommit=True) as admin:
                admin.execute("ALTER SYSTEM SET log_line_prefix = '%m [%p] [%a] '")
                admin.execute("SELECT pg_reload_conf()")
            command = ["cargo", "test", "--locked", "--offline", "--features", "test-fixtures", "--manifest-path", "server/Cargo.toml"]
            suites: list[tuple[list[str], str | None]] = []
            if args.lib:
                suites.append((["--lib"], None))
            elif not args.test:
                suites.append((["--lib", "--bins"], None))
            for test in sorted(tests):
                suites.append((["--test", test], STATEMENT_LOGS.get(test)))
            if len(suites) > 1:
                # Compile all selected binaries together with Cargo's normal
                # build concurrency, then isolate their database mutations.
                selectors = [argument for suite, _ in suites for argument in suite]
                exit_code = run_cargo(command + selectors + ["--no-run"], postgres.url, scratch)
            if not exit_code:
                for index, (suite, statement_log) in enumerate(suites):
                    arguments = command + suite + ["--", "--test-threads=1"]
                    if args.filter:
                        arguments.append(args.filter)
                    extra = {statement_log: str(postgres.log_path)} if statement_log else None
                    with suite_database(postgres, index, bool(statement_log)) as database_url:
                        exit_code = run_cargo(arguments, database_url, scratch, extra)
                    if exit_code:
                        break
        finally:
            postgres.close()
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
