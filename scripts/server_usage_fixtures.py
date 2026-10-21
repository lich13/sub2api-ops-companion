#!/usr/bin/env python3
"""Generate usage-record parity fixtures from the immutable Python reference.

The old tests run from Git SHA 6be7c291ed38dcc337cb88f693639040a3463899.
Their inputs, per-call database snapshots, outputs, and route observations are
captured into tests/contracts/usage/fixtures.json. Rust verification uses an
owned, temporary PostgreSQL cluster and never contacts a configured service.
"""
from __future__ import annotations

import argparse
import ast
import copy
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
REFERENCE_PATHS = ("app", "tests/test_usage_records.py")
DATE_BOUNDARY_MATRIX_ID = "pinned_date_window_timestamp_boundary_matrix"
DATE_BOUNDARY_MATRIX_TEST_ID = (
    "py:tests.test_usage_records:UsageRecordDateTests."
    "test_year_boundary_uses_next_calendar_day_for_end"
)
NATIVE_COMPACTION_HOST_LEGACY_ID = (
    "py:tests.test_usage_records:UsageRecordsTests."
    "test_detail_joins_names_without_dropping_soft_deleted_account"
)
NATIVE_COMPACTION_TEST_NAME = "native_compaction_v2_is_schema_compatible_and_read_only"
NATIVE_COMPACTION_SNAPSHOT_ID = (
    "d013e5a949e6bbab592a8504abed01c6a07a0b6ae17caefd85ff13f78a776e9f"
)
CONTRACT_DIR = ROOT / "tests" / "contracts" / "usage"
FIXTURE_PATH = CONTRACT_DIR / "fixtures.json"
FRAGMENT_PATH = CONTRACT_DIR / "migration-fragment.json"

sys.path.insert(0, str(ROOT / "scripts"))
try:
    from server_credit_contract import safe_environment
    from server_http_contract import TemporaryPostgres
except ImportError as exc:  # pragma: no cover - diagnosed in runner output
    raise SystemExit(f"usage parity dependencies unavailable: {type(exc).__name__}") from exc


class ContractFailure(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractFailure(message)


def archive_reference(destination: Path) -> Path:
    destination.mkdir(mode=0o700, parents=True, exist_ok=True)
    archive_path = destination.parent / "usage-reference.tar"
    with archive_path.open("wb") as archive_file:
        result = subprocess.run(
            ["git", "archive", "--format=tar", REFERENCE_SHA, *REFERENCE_PATHS],
            cwd=ROOT,
            stdout=archive_file,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    require(result.returncode == 0, "fixed Python usage reference could not be read from Git")
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
        raise ContractFailure("fixed Python usage reference could not be extracted") from exc
    finally:
        archive_path.unlink(missing_ok=True)
    test_path = destination / "tests" / "test_usage_records.py"
    require(test_path.is_file(), "pinned usage-record test file is missing")
    (destination / "tests" / "__init__.py").touch(mode=0o600)
    return destination


def legacy_test_ids(reference: Path) -> list[dict[str, str]]:
    tree = ast.parse((reference / "tests/test_usage_records.py").read_text(encoding="utf-8"))
    result = []
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        for child in node.body:
            if isinstance(child, ast.FunctionDef) and child.name.startswith("test_"):
                result.append(
                    {
                        "class_name": node.name,
                        "method_name": child.name,
                        "legacy_test_id": f"py:tests.test_usage_records:{node.name}.{child.name}",
                    }
                )
    require(len(result) == 70, f"pinned usage test count changed: {len(result)}")
    require(len({row["legacy_test_id"] for row in result}) == 70, "legacy usage IDs are not unique")
    return result


def oracle_program() -> str:
    return r'''from __future__ import annotations

import hashlib
import json
import sys
import unittest
from datetime import date, datetime
from decimal import Decimal
from pathlib import Path
from typing import Any

import tests.test_usage_records as legacy
import app.usage_records as usage_module

target = Path(sys.argv[1])
current = {"id": None, "events": []}
snapshots = {}
traces = {}
http_traces = {}
projection_classes = {"ProjectionTests"}
route_classes = {"UsageRecordsRouteTests"}
reused_ids = {
    "py:tests.test_usage_records:UsageRecordsTests.test_cursor_rejects_each_changed_filter_before_querying",
    "py:tests.test_usage_records:UsageRecordDateTests.test_date_cursor_keeps_window_and_rejects_changed_or_omitted_dates",
    "py:tests.test_usage_records:UsageRecordsTests.test_list_count_and_detail_use_bounded_read_only_transactions",
    "py:tests.test_usage_records:UsageRecordsRouteTests.test_usage_records_expose_no_write_method",
}


def packed(value):
    if isinstance(value, (datetime, date)):
        return value.isoformat()
    if isinstance(value, Decimal):
        return str(value)
    if isinstance(value, dict):
        return {str(key): packed(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [packed(item) for item in value]
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return str(value)


POSTGRES_USAGE_BOOLEAN_FIELDS = frozenset(
    ("upstream_model_mismatch", "stream", "openai_ws_mode")
)


def postgres_usage_row(value: dict[str, Any]) -> dict[str, Any]:
    """Convert SQLite integer booleans only on rows persisted as PostgreSQL usage_logs."""
    result = dict(value)
    for field in POSTGRES_USAGE_BOOLEAN_FIELDS:
        raw = result.get(field)
        if type(raw) is int and raw in (0, 1):
            result[field] = bool(raw)
    return result


def postgres_usage_result(operation: str, value: Any) -> Any:
    """Match PostgreSQL BOOL JSON output for database-backed usage rows, not project inputs."""
    if operation == "list" and isinstance(value, dict) and "items" in value:
        result = dict(value)
        result["items"] = [
            postgres_usage_row(row) if isinstance(row, dict) else row
            for row in result.get("items", [])
        ]
        assert result.keys() == value.keys(), "BOOL normalization must preserve response fields"
        return result
    if operation == "detail" and isinstance(value, dict):
        return postgres_usage_row(value)
    return value


def snapshot(db):
    tables = {}
    for table in ("usage_logs", "users", "api_keys", "accounts", "groups"):
        rows = db.raw.execute(f"SELECT * FROM {table} ORDER BY id").fetchall()
        packed_rows = [packed(dict(row)) for row in rows]
        if table == "usage_logs":
            packed_rows = [postgres_usage_row(row) for row in packed_rows]
        tables[table] = packed_rows
    identity = hashlib.sha256(
        json.dumps(tables, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    snapshots.setdefault(identity, tables)
    return identity


def active_id():
    return current["id"] or ""


def active_class():
    value = active_id().split(":", 2)[-1]
    return value.split(".", 1)[0]


def record(event):
    current["events"].append(packed(event))


reference_project = usage_module.project
reference_date_window = usage_module.date_window
reference_timestamp = usage_module.timestamp
reference_list = usage_module.UsageRecords.list
reference_options = usage_module.UsageRecords.options
reference_detail = usage_module.UsageRecords.detail
ReferenceTestClient = legacy.TestClient


def capture_project(row):
    result = reference_project(row)
    if active_class() in projection_classes:
        record({"operation": "project", "input": row, "expected": result})
    return result


def capture_timestamp(value):
    try:
        result = reference_timestamp(value)
    except legacy.HTTPException as error:
        if active_class() in projection_classes:
            record({"operation": "timestamp", "input": value, "error_status": error.status_code})
        raise
    if active_class() in projection_classes:
        record({"operation": "timestamp", "input": value, "expected": result})
    return result


def capture_date_boundary_matrix():
    cases = [
        ("date_year_zero", "date_window", {"start_date": "0000-01-01", "end_date": "0000-01-01"}),
        ("date_year_one_shanghai_utc_underflow", "date_window", {"start_date": "0001-01-01", "end_date": "0001-01-01"}),
        ("date_year_max_end_overflow", "date_window", {"start_date": "9999-12-31", "end_date": "9999-12-31"}),
        ("date_leap_day", "date_window", {"start_date": "2024-02-29", "end_date": "2024-02-29"}),
        ("date_shanghai_1990_dst_start", "date_window", {"start_date": "1990-04-15", "end_date": "1990-04-15"}),
        ("date_shanghai_1919_midnight_gap", "date_window", {"start_date": "1919-04-13", "end_date": "1919-04-13"}),
        ("timestamp_year_zero", "timestamp", "0000-01-01T00:00:00+00:00"),
        ("timestamp_year_one_utc_min", "timestamp", "0001-01-01T00:00:00+00:00"),
        ("timestamp_year_one_utc_underflow", "timestamp", "0001-01-01T00:00:00+08:00"),
        ("timestamp_year_max_utc_overflow", "timestamp", "9999-12-31T23:59:59-01:00"),
        ("timestamp_year_max_utc_max", "timestamp", "9999-12-31T23:59:59+00:00"),
        ("timestamp_leap_day_offset", "timestamp", "2024-02-29T12:34:56+08:00"),
    ]
    captured = []
    for case_id, operation, inputs in cases:
        event = {"id": case_id, "operation": operation, "input": inputs}
        try:
            if operation == "date_window":
                value = reference_date_window(inputs["start_date"], inputs["end_date"])
            else:
                value = reference_timestamp(inputs)
        except legacy.HTTPException as error:
            event["error_status"] = error.status_code
        else:
            event["expected"] = packed(value)
        captured.append(event)
    return {"id": "pinned_date_window_timestamp_boundary_matrix", "cases": captured}


def operation_event(self, operation, inputs, invoke):
    legacy_id = active_id()
    if not legacy_id or active_class() in route_classes or legacy_id in reused_ids:
        return invoke()
    snapshot_id = snapshot(self.db)
    statement_count = len(self.db.statements)
    try:
        result = invoke()
    except legacy.HTTPException as error:
        record({
            "operation": operation,
            "input": inputs,
            "snapshot": snapshot_id,
            "now": legacy.FixedDateTime.current,
            "error_status": error.status_code,
            "database_statement_delta": len(self.db.statements) - statement_count,
        })
        raise
    expected = postgres_usage_result(operation, result)
    record({
        "operation": operation,
        "input": inputs,
        "snapshot": snapshot_id,
        "now": legacy.FixedDateTime.current,
        "expected": expected,
        "database_statement_delta": len(self.db.statements) - statement_count,
    })
    return result


def capture_list(self, *args, **kwargs):
    return operation_event(self, "list", {"kwargs": kwargs}, lambda: reference_list(self, *args, **kwargs))


def capture_options(self, *args, **kwargs):
    return operation_event(self, "options", {"kwargs": kwargs}, lambda: reference_options(self, *args, **kwargs))


def capture_detail(self, *args, **kwargs):
    return operation_event(self, "detail", {"args": args, "kwargs": kwargs}, lambda: reference_detail(self, *args, **kwargs))


class RecordingTestClient:
    def __init__(self, app, *args, **kwargs):
        self.inner = ReferenceTestClient(app, *args, **kwargs)
        self.db = getattr(legacy, "_active_case", None).db

    def get(self, path, **kwargs):
        return self._request("GET", path, **kwargs)

    def request(self, method, path, **kwargs):
        return self._request(str(method).upper(), path, **kwargs)

    def _request(self, method, path, **kwargs):
        legacy_id = active_id()
        if not legacy_id or legacy_id in reused_ids:
            return self.inner.request(method, path, **kwargs)
        snapshot_id = snapshot(self.db)
        statement_count = len(self.db.statements)
        response = self.inner.request(method, path, **kwargs)
        try:
            body = response.json()
        except (ValueError, TypeError):
            body = None
        event = {
            "operation": "http",
            "method": method,
            "path": path,
            "params": kwargs.get("params"),
            "headers": kwargs.get("headers", {}),
            "snapshot": snapshot_id,
            "reference_now": legacy.NOW.isoformat(),
            "expected_status": response.status_code,
            "expected_body": body,
            "expected_headers": {
                name: response.headers.get(name)
                for name in ("cache-control", "x-request-id")
            },
            "database_statement_delta": len(self.db.statements) - statement_count,
        }
        http_traces.setdefault(legacy_id, []).append(packed(event))
        return response

    def close(self):
        return self.inner.close()

    def __getattr__(self, name):
        return getattr(self.inner, name)


legacy._active_case = None
legacy.project = capture_project
legacy.timestamp = capture_timestamp
usage_module.project = capture_project
usage_module.timestamp = capture_timestamp
usage_module.UsageRecords.list = capture_list
usage_module.UsageRecords.options = capture_options
usage_module.UsageRecords.detail = capture_detail
legacy.TestClient = RecordingTestClient

class_names = (
    "ProjectionTests",
    "UsageRecordsTests",
    "UsageRecordDateTests",
    "UsageRecordSummaryTests",
    "UsageRecordOptionsTests",
    "UsageRecordsRouteTests",
)
failures = []
tests_run = 0
for class_name in class_names:
    test_class = getattr(legacy, class_name)
    for name in sorted(value for value in dir(test_class) if value.startswith("test_")):
        legacy_id = f"py:tests.test_usage_records:{class_name}.{name}"
        current["id"] = legacy_id
        current["events"] = []
        legacy._active_case = None
        case = test_class(name)
        original_setup = case.setUp

        def setup_with_active_case(original_setup=original_setup, case=case):
            legacy._active_case = case
            original_setup()

        case.setUp = setup_with_active_case
        result = unittest.TestResult()
        case.run(result)
        tests_run += result.testsRun
        if result.errors or result.failures or result.unexpectedSuccesses:
            detail = " | ".join(
                trace.strip().splitlines()[-1]
                for _, trace in [*result.errors, *result.failures]
            )
            failures.append({"legacy_test_id": legacy_id, "error": detail or "unexpected success"})
        if legacy_id not in reused_ids:
            traces[legacy_id] = current["events"]
        current["id"] = None
        current["events"] = []

output = {
    "schema_version": 1,
    "reference_sha": "6be7c291ed38dcc337cb88f693639040a3463899",
    "legacy_test_count": tests_run,
    "legacy_failures": failures,
    "snapshots": snapshots,
    "traces": traces,
    "http_traces": http_traces,
    "date_boundary_matrix": capture_date_boundary_matrix(),
}
target.write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
if tests_run != 70 or failures:
    print(json.dumps({"tests_run": tests_run, "failures": failures}, ensure_ascii=False), file=sys.stderr)
    raise SystemExit(1)
'''


def run_reference_suite(reference: Path, temporary: Path) -> dict[str, Any]:
    target = temporary / "usage-reference-fixtures.json"
    program = temporary / "generate_usage_reference.py"
    program.write_text(oracle_program(), encoding="utf-8")
    env = safe_environment("", temporary)
    env.update(
        {
            "PYTHONPATH": str(reference),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
    )
    result = subprocess.run(
        [sys.executable, str(program), str(target)],
        cwd=reference,
        env=env,
        capture_output=True,
        text=True,
        timeout=180,
        check=False,
    )
    if result.returncode:
        details = (result.stdout + result.stderr).replace(str(ROOT), "<repository>").replace(str(reference), "<reference>")
        details = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", details)
        details = " ".join(details.split())
        raise ContractFailure(f"pinned Python usage tests failed with exit {result.returncode}: {details[-5000:]}")
    fixtures = json.loads(target.read_text(encoding="utf-8"))
    require(fixtures.get("reference_sha") == REFERENCE_SHA, "usage fixtures used the wrong Git SHA")
    require(fixtures.get("legacy_test_count") == 70, "pinned usage suite did not execute all 70 tests")
    require(fixtures.get("legacy_failures") == [], "pinned usage suite reported failures")
    return fixtures


def class_slug(class_name: str) -> str:
    return {
        "ProjectionTests": "projection",
        "UsageRecordsTests": "records",
        "UsageRecordDateTests": "date",
        "UsageRecordSummaryTests": "summary",
        "UsageRecordOptionsTests": "options",
        "UsageRecordsRouteTests": "route",
    }[class_name]


def rust_test_name(test: dict[str, str]) -> str:
    suffix = test["method_name"].removeprefix("test_")
    return f"legacy_{class_slug(test['class_name'])}_{suffix}"


def rust_test_call_anchor(source: str, test_name: str, helper: str, legacy_id: str) -> str:
    declaration = re.search(
        rf"async fn\s+{re.escape(test_name)}\s*\([^)]*\)\s*\{{",
        source,
    )
    require(declaration is not None, f"explicit Rust test is missing for {legacy_id}")
    body_end = source.find("\n}", declaration.end())
    require(body_end >= 0, f"explicit Rust test body is incomplete for {legacy_id}")
    body = source[declaration.end() : body_end]
    call = re.search(rf"{re.escape(helper)}\([\s\S]*?\)\s*\.await;", body)
    require(call is not None, f"Rust test does not call its parity helper for {legacy_id}")
    anchor = call.group(0).strip()
    require(f'"{legacy_id}"' in anchor, f"Rust helper call uses the wrong legacy ID for {legacy_id}")
    return anchor


def native_compaction_supporting_target() -> dict[str, Any]:
    return {
        "kind": "rust_test",
        "path": "server/tests/usage_records_parity.rs",
        "test_name": NATIVE_COMPACTION_TEST_NAME,
        "assertions": [
            "assert_eq!(missing_false_count, missing_items.len());",
            'assert_eq!(missing_detail["native_compaction_v2"], json!(false));',
            "assert_eq!(present_values, vec![(1, true), (2, false), (3, false)]);",
            "assert_eq!(list_item, &detail);",
            "assert_eq!(error_status(failed), Err(500));",
        ],
        "helper_assertions": [
            {
                "name": "assert_read_only_queries",
                "assertions": [
                    'sql.starts_with("select ")',
                    '!sql.contains("credentials")',
                    '!sql.contains("request_body")',
                ],
            },
            {
                "name": "assert_native_compaction_probe",
                "assertions": [
                    'assert_eq!(probes, 1, "{context}: expected one native compaction catalog probe");',
                ],
            },
        ],
        "fixtures": [
            {
                "path": "tests/contracts/usage/fixtures.json",
                "ids": [NATIVE_COMPACTION_SNAPSHOT_ID],
            }
        ],
    }


def create_migration_fragment(reference: Path, fixture: dict[str, Any]) -> dict[str, Any]:
    tests = legacy_test_ids(reference)
    query_fragment = json.loads(
        (ROOT / "tests/contracts/query/migration-fragment.json").read_text(encoding="utf-8")
    )
    require(query_fragment.get("reference_sha") == REFERENCE_SHA, "query reuse map is from a different reference")
    reusable = {
        row["legacy_test_id"]: row
        for row in query_fragment.get("mappings", [])
        if row.get("status") == "mapped"
        and row.get("legacy_test_id", "").startswith("py:tests.test_usage_records:")
    }
    mappings = []
    rust_source = (ROOT / "server/tests/usage_records_parity.rs").read_text(encoding="utf-8")
    for test in tests:
        legacy_id = test["legacy_test_id"]
        if legacy_id in reusable:
            mapping = copy.deepcopy(reusable[legacy_id])
            mapping["reuse"] = {
                "fragment": "tests/contracts/query/migration-fragment.json",
                "scope": mapping["coverage_note"],
            }
            mappings.append(mapping)
            continue
        events = fixture.get("http_traces", {}).get(legacy_id, []) if test["class_name"] == "UsageRecordsRouteTests" else fixture.get("traces", {}).get(legacy_id, [])
        require(bool(events), f"pinned fixture trace is empty for {legacy_id}")
        test_name = rust_test_name(test)
        helper = "replay_route_case" if test["class_name"] == "UsageRecordsRouteTests" else "replay_direct_case"
        test_call = rust_test_call_anchor(rust_source, test_name, helper, legacy_id)
        operation_types = sorted({event["operation"] for event in events})
        route_clock_scope = (
            "HTTP trace preserves the legacy NOW and runs Runtime with that fixed clock so the default window remains deterministic. "
            if helper == "replay_route_case"
            else ""
        )
        if helper == "replay_route_case":
            helper_assertions = [
                {
                    "name": helper,
                    "assertions": [
                        'assert!(\n        events\n            .iter()\n            .all(|event| event["reference_now"].as_str() == Some(reference_now_text)),\n        "legacy route trace has one fixed reference time: {legacy_id}"\n    );',
                        'assert_eq!(\n                observed_at, reference_now_text,\n                "legacy route reference time {legacy_id}; params={:?}",\n                event["params"]\n            );',
                        'assert_eq!(\n            response.status().as_u16(),\n            expected_status,\n            "legacy route {legacy_id}: {path}; params={:?}",\n            event["params"]\n        );',
                        'assert_eq!(\n                stable_response(&actual_body),\n                stable_response(&event["expected_body"]),\n                "legacy route body {legacy_id}: {path}; params={:?}",\n                event["params"]\n            );',
                        'assert_eq!(\n                statements.len(),\n                0,\n                "legacy route rejected before database reads: {legacy_id}; path={path}; params={:?}",\n                event["params"]\n            );',
                        "assert_read_only_queries(&statements, &context);",
                    ],
                }
            ]
        elif test["class_name"] == "ProjectionTests":
            helper_assertions = [
                {
                    "name": helper,
                    "assertions": [
                        'assert_native_compaction_record(&actual, &format!("legacy projection {legacy_id}"));',
                        'let comparable = strip_native_compaction_record(&actual);',
                        'assert_eq!(comparable, event["expected"], "legacy projection {legacy_id}");',
                        'assert_eq!(\n                        actual.map_err(|error| error.status.as_u16()),\n                        Err(status as u16),\n                        "legacy timestamp {legacy_id}"\n                    );',
                    ],
                }
            ]
        else:
            helper_assertions = [
                {
                    "name": helper,
                    "assertions": [
                        'assert_eq!(\n                        actual, event["expected"],\n                        "legacy operation {legacy_id}; input={:?}",\n                        event["input"]\n                    );',
                        'assert_eq!(\n                        statements.len(),\n                        0,\n                        "legacy rejected before SQL: {legacy_id}; input={:?}",\n                        event["input"]\n                    );',
                        'assert_eq!(\n                        error_status(result),\n                        Err(status as u16),\n                        "legacy service error {legacy_id}; input={:?}",\n                        event["input"]\n                    );',
                        "assert_read_only_queries(&statements, &context);",
                    ],
                }
            ]
        if test["class_name"] != "ProjectionTests":
            helper_assertions.extend([
                {
                    "name": "operation_statements",
                    "assertions": [
                        'assert!(before < after, "SQL log barriers are out of order");',
                        ".filter(|statement| !is_pool_session_setup_statement(statement))",
                    ],
                },
                {
                    "name": "assert_read_only_queries",
                    "assertions": [
                        'sql.starts_with("select ")',
                        '!sql.contains("credentials")',
                        '!sql.contains("request_body")',
                    ],
                },
            ])
            helper_assertions.append(
                {
                    "name": "is_pool_session_setup_statement",
                    "assertions": [
                        """matches!(
        statement,
        "set time zone 'utc'" | "set default_transaction_read_only = on"
    )"""
                    ],
                }
            )
        boundary_matrix_scope = ""
        if legacy_id == DATE_BOUNDARY_MATRIX_TEST_ID:
            helper_assertions.append(
                {
                    "name": "pinned_date_window_timestamp_boundary_matrix",
                    "assertions": [
                        'assert_eq!(matrix["id"], "pinned_date_window_timestamp_boundary_matrix");',
                        "assert_eq!(cases.len(), 12);",
                        'assert_eq!(actual, expected, "boundary case {case_id}; input={input}");',
                    ],
                }
            )
            boundary_matrix_scope = (
                "A supplemental 12-case matrix captures date_window/timestamp outputs and errors from the pinned Python functions, "
                "then compares every item directly in Rust, including year bounds, leap day, 1990 DST, and the 1919 Shanghai midnight gap."
            )
        if legacy_id == (
            "py:tests.test_usage_records:UsageRecordSummaryTests."
            "test_summary_and_rows_share_read_transaction_and_watermark_during_concurrent_write"
        ):
            helper_assertions.append(
                {
                    "name": "assert_summary_snapshot_with_concurrent_write",
                    "assertions": [
                        'assert_eq!(writer_sum, "103.1");',
                        'assert_eq!(\n        first["latest_id"], 2,\n        "watermark uses the pre-write snapshot"\n    );',
                        'assert_eq!(first["summary"], json!({"actual_cost":"0.3"}));',
                        'assert_eq!(first["items"][0]["id"], 2);',
                        'assert_eq!(first["items"][0]["actual_cost"], "0.2");',
                        'assert_eq!(page["latest_id"], 2, "cursor preserves the old watermark");',
                        'assert_eq!(page["items"][0]["id"], 1);',
                        'assert_eq!(new_count["new_count"], 1);',
                        'assert_eq!(new_count["latest_id"], 3);',
                        'assert_eq!(refreshed["summary"]["actual_cost"], "103.1");',
                    ],
                }
            )
            coverage_scope = (
                "trace 只回放每次调用前的数据库快照，不单独作为并发覆盖；同一 Rust 测试另在隔离 PostgreSQL 中用 advisory lock 阻塞 summary SELECT，"
                "期间由另一连接提交新增与更新，再核对首屏 summary、items、watermark、游标续页和刷新结果。"
            )
        else:
            coverage_scope = ""
        fixture_ids = [legacy_id]
        fixture_reference = {
            "path": "tests/contracts/usage/fixtures.json",
            "ids": fixture_ids,
        }
        if legacy_id == DATE_BOUNDARY_MATRIX_TEST_ID:
            fixture_ids.append(DATE_BOUNDARY_MATRIX_ID)
            fixture_reference["json_paths"] = ["date_boundary_matrix.cases"]
        mappings.append(
            {
                "legacy_test_id": legacy_id,
                "status": "mapped",
                "target": {
                    "kind": "rust_test",
                    "path": "server/tests/usage_records_parity.rs",
                    "test_name": test_name,
                    "assertions": [test_call],
                    "helper_assertions": helper_assertions,
                    "fixtures": [fixture_reference],
                },
                "coverage_note": (
                    f"从固定 SHA 旧测试逐次捕获 {len(events)} 个 {', '.join(operation_types)} 观测，"
                    "Rust 在隔离 PostgreSQL/loopback 上以相同输入和各调用前数据库快照回放；"
                    "成功响应比较稳定字段完整 JSON，错误比较 HTTP 状态，另核对只读 SQL 与隐私边界。"
                    f"{route_clock_scope}{coverage_scope}{boundary_matrix_scope}"
                ),
            }
        )
        if legacy_id == NATIVE_COMPACTION_HOST_LEGACY_ID:
            mappings[-1]["target"]["supporting_targets"] = [
                native_compaction_supporting_target()
            ]
            mappings[-1]["coverage_note"] += (
                " Supporting Rust coverage probes the compatibility column on every list/detail read, "
                "covers absent/true/false/NULL schemas, list/detail parity, read-only SQL, and propagated SQL errors."
            )
    require(len(mappings) == 70, f"migration mapping count changed: {len(mappings)}")
    require(len({row["legacy_test_id"] for row in mappings}) == 70, "migration fragment has duplicate legacy IDs")
    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "legacy_test_count": 70,
        "mappings": mappings,
        "uncovered_notes": [],
    }


def write_fixture_files(reference: Path, fixture: dict[str, Any]) -> None:
    CONTRACT_DIR.mkdir(parents=True, exist_ok=True)
    fragment = create_migration_fragment(reference, fixture)
    FIXTURE_PATH.write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    FRAGMENT_PATH.write_text(json.dumps(fragment, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def enable_postgres_logging(postgres: TemporaryPostgres) -> None:
    import psycopg

    admin = (
        f"host=127.0.0.1 port={postgres.port} user=contract_fixture "
        "dbname=postgres sslmode=disable connect_timeout=2"
    )
    with psycopg.connect(admin, autocommit=True) as connection:
        connection.execute("ALTER SYSTEM SET log_statement = 'all'")
        connection.execute("ALTER SYSTEM SET log_line_prefix = '%m [%p] [%a] '")
        connection.execute("SELECT pg_reload_conf()")


def verify_reference_postgres_boolean_types(
    reference: Path,
    fixture: dict[str, Any],
    postgres: TemporaryPostgres,
) -> None:
    """Check the pinned Python UsageRecords against PostgreSQL BOOL columns."""
    import importlib.util
    from contextlib import contextmanager

    import psycopg
    from psycopg.rows import dict_row

    schema = "usage_records_reference_probe"
    legacy_id = (
        "py:tests.test_usage_records:UsageRecordsTests."
        "test_request_type_filter_matches_legacy_and_explicit_semantics"
    )
    schema_path = CONTRACT_DIR / "schema.sql"
    module_path = reference / "app" / "usage_records.py"
    spec = importlib.util.spec_from_file_location("usage_records_reference_probe", module_path)
    require(spec is not None and spec.loader is not None, "pinned UsageRecords module is missing")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    admin_dsn = postgres.url
    options = f"-csearch_path={schema}"
    try:
        with psycopg.connect(admin_dsn, autocommit=True) as admin:
            admin.execute(f"DROP SCHEMA IF EXISTS {schema} CASCADE")
            admin.execute(f"CREATE SCHEMA {schema}")
        with psycopg.connect(admin_dsn, autocommit=True, options=options) as connection:
            for statement in schema_path.read_text(encoding="utf-8").split(";"):
                if statement.strip():
                    connection.execute(statement)
            snapshot_id = fixture["traces"][legacy_id][0]["snapshot"]
            rows = fixture["snapshots"][snapshot_id]["usage_logs"]
            for row in rows:
                connection.execute(
                    "INSERT INTO usage_logs "
                    "SELECT * FROM jsonb_populate_record(NULL::usage_logs, %s::jsonb)",
                    (json.dumps(row, ensure_ascii=False),),
                )

        class PostgresReferenceDatabase:
            @contextmanager
            def connection(self):
                with psycopg.connect(
                    admin_dsn,
                    application_name="usage-reference-bool-probe",
                    options=options,
                    row_factory=dict_row,
                ) as connection:
                    yield connection

        service = module.UsageRecords(PostgresReferenceDatabase())
        expected_ids = {"sync": [4, 1], "stream": [5, 2], "ws_v2": [6, 3]}
        for request_type, ids in expected_ids.items():
            result = service.list(
                request_type=request_type,
                from_at="2026-09-30T11:00:00+00:00",
                to_at="2026-09-30T13:00:00+00:00",
            )
            items = result["items"]
            require(
                [row["id"] for row in items] == ids,
                f"pinned PostgreSQL request_type result changed for {request_type}",
            )
            require(
                all(type(row["stream"]) is bool and type(row["openai_ws_mode"]) is bool for row in items),
                f"pinned PostgreSQL {request_type} BOOL fields are not Python bools",
            )
    finally:
        with psycopg.connect(admin_dsn, autocommit=True) as admin:
            admin.execute(f"DROP SCHEMA IF EXISTS {schema} CASCADE")


def run_rust_test(postgres: TemporaryPostgres, temporary: Path) -> int:
    env = safe_environment(postgres.url, temporary)
    env.pop("SUB2OPS_CREDIT_CONTRACT", None)
    env.update(
        {
            "SERVER_USAGE_PARITY_PG_LOG": str(postgres.log_path),
            "RUST_BACKTRACE": "0",
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
        "usage_records_parity",
        "--",
        "--test-threads=1",
    ]
    try:
        result = subprocess.run(
            command,
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            timeout=1200,
            check=False,
        )
    except subprocess.TimeoutExpired:
        print("Rust usage-record parity target: timed out after 1200s", file=sys.stderr)
        return 124
    output = (result.stdout + result.stderr).replace(str(ROOT), "<repository>").replace(str(temporary), "<temporary>")
    output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
    if output:
        print(output, end="" if output.endswith("\n") else "\n")
    print(f"Rust usage-record parity target: exit {result.returncode}")
    return result.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare-only", action="store_true", help="generate fixtures and migration map without running Cargo")
    parser.add_argument("--postgres-bin-dir")
    args = parser.parse_args()
    exit_code = 1
    postgres: TemporaryPostgres | None = None
    temporary_root: Path | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="sub2ops-usage-parity-") as directory:
            temporary = Path(directory)
            temporary.chmod(0o700)
            temporary_root = temporary
            reference = archive_reference(temporary / "reference")
            tests = legacy_test_ids(reference)
            fixture = run_reference_suite(reference, temporary)
            require(len(tests) == fixture["legacy_test_count"] == 70, "legacy test count mismatch")
            write_fixture_files(reference, fixture)
            print(f"pinned Python usage tests: {fixture['legacy_test_count']} passed; reference {REFERENCE_SHA}")
            print(
                "generated usage fixtures: "
                f"{len(fixture['traces']) + len(fixture['http_traces'])} legacy traces, "
                f"{len(fixture['snapshots'])} deduplicated PostgreSQL snapshots"
            )
            if args.prepare_only:
                exit_code = 0
            else:
                postgres = TemporaryPostgres(temporary, args.postgres_bin_dir)
                try:
                    postgres.start()
                    enable_postgres_logging(postgres)
                    verify_reference_postgres_boolean_types(reference, fixture, postgres)
                    exit_code = run_rust_test(postgres, temporary)
                finally:
                    postgres.close()
    except Exception as exc:
        detail = str(exc) if isinstance(exc, ContractFailure) else type(exc).__name__
        print(f"usage-record fixture runner failed: {detail}", file=sys.stderr)
        exit_code = 1
    removed = temporary_root is None or not temporary_root.exists()
    pg_stopped = postgres is None or not postgres.started
    print(
        "cleanup: temporary PostgreSQL stopped; pinned source, state, log, and scratch removed"
        if removed and pg_stopped
        else f"cleanup: temporary_root_removed={removed}, postgres_stopped={pg_stopped}"
    )
    print(f"usage-record fixture runner: exit {exit_code}")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
