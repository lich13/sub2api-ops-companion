#!/usr/bin/env python3
"""Run the query-history contract against pinned Python and an owned PostgreSQL."""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

from server_credit_contract import safe_environment
from server_http_contract import ROOT, TemporaryPostgres

from server_reference import REFERENCE_SHA
REFERENCE_PATHS = (
    "app",
    "tests/test_postgres_events030.py",
    "tests/test_events030.py",
    "tests/test_usage_records.py",
    "tests/test_oauth_queries.py",
)
LEGACY_TESTS = (
    "tests.test_events030.EventQuery030Tests",
    "tests.test_events030.EventRoutes030Tests",
    "tests.test_postgres_events030.PostgreSQLEvents030Tests.test_categories_are_disjoint_and_complete_except_explicit_local_limits",
    "tests.test_postgres_events030.PostgreSQLEvents030Tests.test_default_category_executes_the_legacy_predicate",
    "tests.test_postgres_events030.PostgreSQLEvents030Tests.test_detail_uses_same_category_and_preserves_missing_or_deleted_entities",
    "tests.test_postgres_events030.PostgreSQLEvents030Tests.test_list_pagination_filters_before_limit_and_preserves_account_filter",
    "tests.test_usage_records.UsageRecordsTests.test_cursor_rejects_each_changed_filter_before_querying",
    "tests.test_usage_records.UsageRecordDateTests.test_date_cursor_keeps_window_and_rejects_changed_or_omitted_dates",
    "tests.test_usage_records.UsageRecordsTests.test_list_count_and_detail_use_bounded_read_only_transactions",
    "tests.test_usage_records.UsageRecordsRouteTests.test_usage_records_expose_no_write_method",
    "tests.test_oauth_queries.OAuthQueryCoordinatorTests.test_failures_back_off_one_three_six_then_twelve_hours",
    "tests.test_oauth_queries.OAuthQueryCoordinatorTests.test_reservation_is_on_disk_before_http_and_timeout_consumes_attempt",
)


class ContractFailure(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractFailure(message)


def sanitized(output: str, temporary: str) -> str:
    output = output.replace(str(ROOT), "<repository>").replace(temporary, "<temporary>")
    return re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)


def archive_reference(destination: Path) -> None:
    destination.mkdir(mode=0o700, parents=True, exist_ok=True)
    archive_path = destination.parent / "query-reference.tar"
    with archive_path.open("wb") as archive_file:
        result = subprocess.run(
            ["git", "archive", "--format=tar", REFERENCE_SHA, *REFERENCE_PATHS],
            cwd=ROOT,
            stdout=archive_file,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    require(result.returncode == 0, "fixed Python query reference could not be read from Git")
    try:
        root = destination.resolve()
        with tarfile.open(archive_path, mode="r:") as archive:
            for member in archive.getmembers():
                relative = Path(member.name)
                target = (destination / relative).resolve()
                require(
                    not relative.is_absolute()
                    and ".." not in relative.parts
                    and target.is_relative_to(root),
                    "fixed reference archive contains an unsafe path",
                )
                if member.isdir():
                    target.mkdir(mode=0o700, parents=True, exist_ok=True)
                    continue
                require(member.isfile(), "fixed reference archive contains a non-file member")
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                source = archive.extractfile(member)
                require(source is not None, "fixed reference archive contains an unreadable file")
                with source, target.open("xb") as extracted:
                    shutil.copyfileobj(source, extracted)
                target.chmod(member.mode & 0o755)
    except (OSError, tarfile.TarError) as exc:
        raise ContractFailure("fixed Python query reference could not be extracted") from exc
    finally:
        archive_path.unlink(missing_ok=True)
    require((destination / "app" / "desktop_api.py").is_file(), "pinned desktop API is missing")
    require(
        (destination / "tests" / "test_postgres_events030.py").is_file(),
        "pinned PostgreSQL event test is missing",
    )
    # The baseline stores tests as a namespace package. Add only an empty
    # temporary package marker so unittest cannot resolve an unrelated site package.
    (destination / "tests" / "__init__.py").touch(mode=0o600)


def isolated_environment(database_url: str, temporary: Path) -> dict[str, str]:
    env = safe_environment(database_url, temporary)
    for name in tuple(env):
        if name.startswith("PG"):
            env.pop(name, None)
    return env


def run(command: list[str], *, cwd: Path, env: dict[str, str], temporary: str, label: str,
        timeout_seconds: int) -> int:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
            check=False,
        )
    except subprocess.TimeoutExpired:
        print(f"{label}: timed out after {timeout_seconds}s", file=sys.stderr)
        return 124
    output = sanitized(result.stdout + result.stderr, temporary)
    if output:
        print(output, end="" if output.endswith("\n") else "\n")
    print(f"{label}: exit {result.returncode}")
    return result.returncode


def create_legacy_database(postgres: TemporaryPostgres) -> None:
    import psycopg

    admin = (
        f"host=127.0.0.1 port={postgres.port} user=contract_fixture "
        "dbname=postgres sslmode=disable connect_timeout=2"
    )
    with psycopg.connect(admin, autocommit=True) as connection:
        connection.execute("CREATE DATABASE sub2ops_qa")
        connection.execute("ALTER SYSTEM SET log_statement = 'all'")
        connection.execute("ALTER SYSTEM SET log_line_prefix = '%m [%p] [%a] '")
        connection.execute("SELECT pg_reload_conf()")


def run_legacy_tests(reference: Path, postgres: TemporaryPostgres, temporary: Path) -> int:
    env = isolated_environment("", temporary)
    env.update(
        {
            "PYTHONPATH": str(reference),
            "PYTHONDONTWRITEBYTECODE": "1",
            "SUB2OPS_TEST_DATABASE_URL": (
                f"postgresql://contract_fixture@127.0.0.1:{postgres.port}/sub2ops_qa"
            ),
        }
    )
    command = [sys.executable, "-m", "unittest", *LEGACY_TESTS]
    return run(
        command,
        cwd=reference,
        env=env,
        temporary=str(temporary),
        label="pinned Python query selectors",
        timeout_seconds=90,
    )


def oracle_generator() -> str:
    return r'''import json
import sys
from pathlib import Path
from fastapi import HTTPException

from tests.test_postgres_events030 import PostgreSQLEvents030Tests

target = Path(sys.argv[1])
case = PostgreSQLEvents030Tests(
    "test_list_pagination_filters_before_limit_and_preserves_account_filter"
)
case.setUp()
try:
    case.connection.execute("UPDATE ops_error_logs SET created_at='2026-10-08T03:00:00Z'")
    accounts = []
    for row in case.connection.execute(
        "SELECT id,name,platform,type,deleted_at FROM accounts ORDER BY id"
    ).fetchall():
        accounts.append({
            "id": row["id"], "name": row["name"], "platform": row["platform"],
            "type": row["type"],
            "deleted_at": row["deleted_at"].isoformat() if row["deleted_at"] else None,
        })
    groups = []
    for row in case.connection.execute(
        "SELECT id,name,deleted_at FROM groups ORDER BY id"
    ).fetchall():
        groups.append({
            "id": row["id"], "name": row["name"],
            "deleted_at": row["deleted_at"].isoformat() if row["deleted_at"] else None,
        })
    combinations = []
    for category in ("degradation", "other", None):
        for account_id in (None, 1, 5, 99):
            pages, seen, cursor = [], [], None
            expected = sorted(
                (event_id for event_id in case.expected[category]
                 if account_id is None or case.rows[event_id]["account_id"] == account_id),
                reverse=True,
            )
            for _ in range(len(case.rows) + 1):
                page = case.service.errors(account_id, cursor, 3, category=category)
                ids = [item["id"] for item in page["items"]]
                pages.append({"ids": ids, "next_cursor": page["next_cursor"]})
                seen.extend(ids)
                if page["next_cursor"] is None:
                    break
                cursor = page["next_cursor"]
            else:
                raise AssertionError("pinned Python pagination did not terminate")
            if seen != expected:
                raise AssertionError("pinned Python page output disagrees with its expected set")
            combinations.append({
                "category": category,
                "account_id": account_id,
                "expected_ids": expected,
                "pages": pages,
            })
    details = []
    for category in ("degradation", "other", None):
        for event_id in case.rows:
            try:
                value = case.service.error_detail(event_id, category=category)
                details.append({"category": category, "id": event_id, "status": 200, "value": value})
            except HTTPException as exc:
                if exc.status_code != 404:
                    raise
                details.append({"category": category, "id": event_id, "status": 404})
    output = {
        "reference_sha": "6be7c291ed38dcc337cb88f693639040a3463899",
        "accounts": accounts,
        "groups": groups,
        "rows": list(case.rows.values()),
        "combinations": combinations,
        "details": details,
        "names": case.names,
    }
    target.write_text(json.dumps(output, ensure_ascii=False, default=lambda value: value.isoformat()), encoding="utf-8")
finally:
    case.doCleanups()
'''


def build_reference_pages(reference: Path, postgres: TemporaryPostgres, temporary: Path) -> Path:
    script = temporary / "generate_query_reference.py"
    output = temporary / "query-reference-pages.json"
    script.write_text(oracle_generator(), encoding="utf-8")
    env = isolated_environment("", temporary)
    env.update(
        {
            "PYTHONPATH": str(reference),
            "PYTHONDONTWRITEBYTECODE": "1",
            "SUB2OPS_TEST_DATABASE_URL": (
                f"postgresql://contract_fixture@127.0.0.1:{postgres.port}/sub2ops_qa"
            ),
        }
    )
    status = run(
        [sys.executable, str(script), str(output)],
        cwd=reference,
        env=env,
        temporary=str(temporary),
        label="pinned Python pagination oracle",
        timeout_seconds=30,
    )
    require(status == 0 and output.is_file(), "pinned Python pagination oracle generation failed")
    oracle = json.loads(output.read_text(encoding="utf-8"))
    require(oracle.get("reference_sha") == REFERENCE_SHA, "pagination oracle used the wrong source SHA")
    require(len(oracle.get("combinations", [])) == 12, "pagination oracle must contain all 12 scopes")
    return output


def run_rust_test(oracle_path: Path, postgres: TemporaryPostgres, temporary: Path) -> int:
    contract_dir = temporary / "rust-state"
    contract_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    env = isolated_environment(postgres.url, temporary)
    env.update(
        {
            "SERVER_QUERY_HISTORY_DIR": str(contract_dir),
            "SERVER_QUERY_HISTORY_REFERENCE_PAGES": str(oracle_path),
            "SERVER_QUERY_HISTORY_PG_LOG": str(postgres.log_path),
        }
    )
    command = [
        "cargo",
        "test",
        "--locked",
        "--offline",
        "--manifest-path",
        "server/Cargo.toml",
        "--test",
        "query_history_contract",
        "--",
        "--test-threads=1",
    ]
    return run(
        command,
        cwd=ROOT,
        env=env,
        temporary=str(temporary),
        label="Rust query-history contract selectors",
        timeout_seconds=900,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-bin-dir")
    args = parser.parse_args()
    exit_code = 1
    temporary_root: Path | None = None
    postgres: TemporaryPostgres | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="sub2ops-query-history-") as directory:
            temporary = Path(directory)
            temporary_root = temporary
            temporary.chmod(0o700)
            reference = temporary / "reference"
            archive_reference(reference)
            postgres = TemporaryPostgres(temporary, args.postgres_bin_dir)
            try:
                postgres.start()
                create_legacy_database(postgres)
                legacy_status = run_legacy_tests(reference, postgres, temporary)
                if legacy_status:
                    exit_code = legacy_status
                else:
                    oracle_path = build_reference_pages(reference, postgres, temporary)
                    exit_code = run_rust_test(oracle_path, postgres, temporary)
            finally:
                postgres.close()
    except Exception as exc:
        print(f"query-history runner failed: {type(exc).__name__}", file=sys.stderr)
        exit_code = 1
    removed = temporary_root is None or not temporary_root.exists()
    pg_stopped = postgres is None or not postgres.started
    print(
        "cleanup: PostgreSQL stopped; temporary reference, state, log, and oracle removed"
        if removed and pg_stopped
        else f"cleanup: temporary_root_removed={removed}, postgres_stopped={pg_stopped}"
    )
    print(f"query-history runner: exit {exit_code}")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
