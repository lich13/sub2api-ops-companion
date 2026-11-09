#!/usr/bin/env python3
"""Run the retired service's regression suite from its immutable Git revision."""
from __future__ import annotations

import argparse
import ast
import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit

import psycopg

from server_http_contract import REFERENCE_SHA, ROOT, TemporaryPostgres


CONCURRENCY_TEST = "test_concurrency_one_two_three_peaks_and_no_raw_content_in_summary"
CONCURRENCY_FIXTURE_BEFORE = """        for slots in (1, 2, 3):
            active, peak, count = 0, 0, 0
            async def run(**kwargs):
                nonlocal active, peak, count
                active += 1; peak = max(peak, active); count += 1
                try: await asyncio.sleep(.035); return OUTPUT, 'gpt-6-luna'
                finally: active -= 1
"""
CONCURRENCY_FIXTURE_AFTER = """        for slots in (1, 2, 3):
            active, peak, count = 0, 0, 0
            first_batch_ready = asyncio.Event()
            async def run(**kwargs):
                nonlocal active, peak, count
                active += 1; peak = max(peak, active); count += 1
                if active == slots:
                    first_batch_ready.set()
                try:
                    await asyncio.wait_for(first_batch_ready.wait(), timeout=5)
                    return OUTPUT, 'gpt-6-luna'
                finally: active -= 1
"""
FATAL_TEST = "test_fatal_error_cancels_peers_without_retry"
FATAL_FIXTURE_BEFORE = """        calls = 0
        async def run(**kwargs):
            nonlocal calls
            calls += 1
            if calls == 1:
                await asyncio.sleep(.04)
                raise TestFailure('rate_limited')
            await asyncio.sleep(30)
"""
FATAL_FIXTURE_AFTER = """        calls = 0
        peers_started = asyncio.Event()
        async def run(**kwargs):
            nonlocal calls
            calls += 1
            if calls == 3:
                peers_started.set()
            if calls == 1:
                await asyncio.wait_for(peers_started.wait(), timeout=5)
                raise TestFailure('rate_limited')
            await asyncio.sleep(30)
"""


def _sync_concurrency_method(
    reference: Path, relative_path: str, class_name: str, name: str, before: str, after: str
) -> None:
    test_path = reference / relative_path
    source = test_path.read_bytes().decode("utf-8")
    method = _concurrency_method(source, class_name, name)
    if method.end_lineno is None:
        raise RuntimeError(f"Cannot locate the end of {name}")
    lines = source.splitlines(keepends=True)
    method_start, method_end = method.lineno - 1, method.end_lineno
    method_source = "".join(lines[method_start:method_end])
    matches = method_source.count(before)
    if matches != 1:
        raise RuntimeError(
            f"Expected one historical concurrency fixture in {relative_path}; found {matches}"
        )

    patched_method = method_source.replace(before, after, 1)
    patched_source = (
        "".join(lines[:method_start])
        + patched_method
        + "".join(lines[method_end:])
    )
    original_assertions = _self_assertions(method)
    patched_assertions = _self_assertions(_concurrency_method(patched_source, class_name, name))
    if original_assertions != patched_assertions:
        raise RuntimeError(f"{name} assertion AST changed during fixture sync")
    test_path.write_bytes(patched_source.encode("utf-8"))


CANCEL_TEST = "test_cancel_stops_active_streams_and_waiting_retry"
CANCEL_FIXTURE_BEFORE = """        started = asyncio.Event(); cancelled = []
        async def run(**kwargs):
            started.set()
            try: await asyncio.sleep(30)
            except asyncio.CancelledError: cancelled.append(True); raise
"""
CANCEL_FIXTURE_AFTER = """        started = asyncio.Event(); cancelled = []
        active = 0
        async def run(**kwargs):
            nonlocal active
            active += 1
            if active == 3:
                started.set()
            try: await asyncio.sleep(30)
            except asyncio.CancelledError: cancelled.append(True); raise
"""
PARENT_SIBLING_TEST = "test_parent_siblings_and_same_account_never_overlap"
PARENT_SIBLING_FIXTURE_BEFORE = """        self.gates = {1: threading.Event(), 4: threading.Event()}
        first = await self.submit([1, 2, 3, 4])
"""
PARENT_SIBLING_FIXTURE_AFTER = """        self.gates = {1: threading.Event(), 4: threading.Event()}
        parent_locked = asyncio.Event()
        orig_prepare = self.service.actions.prepare_test
        async def ordered_prepare(aid, request):
            if aid in {2, 3}:
                await asyncio.wait_for(parent_locked.wait(), timeout=5)
            try:
                return await orig_prepare(aid, request)
            finally:
                if aid == 1:
                    parent_locked.set()
        self.service.actions.prepare_test = ordered_prepare
        first = await self.submit([1, 2, 3, 4])
"""


def _concurrency_method(source: str, class_name: str, name: str) -> ast.AsyncFunctionDef:
    tree = ast.parse(source)
    matches = [
        method
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == class_name
        for method in node.body
        if isinstance(method, ast.AsyncFunctionDef) and method.name == name
    ]
    if len(matches) != 1:
        raise RuntimeError(f"Expected one {name} fixture; found {len(matches)}")
    return matches[0]


def _self_assertions(method: ast.AsyncFunctionDef) -> tuple[str, ...]:
    calls = [
        node
        for node in ast.walk(method)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr.startswith("assert")
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "self"
    ]
    calls.sort(key=lambda node: (node.lineno, node.col_offset))
    return tuple(ast.dump(node, include_attributes=False) for node in calls)


def apply_concurrency_fixture_sync(reference: Path) -> None:
    for relative_path, class_name, name, before, after in (
        ("tests/test_modeltrace022.py", "ModelConcurrencyTests", CONCURRENCY_TEST, CONCURRENCY_FIXTURE_BEFORE, CONCURRENCY_FIXTURE_AFTER),
        ("tests/test_modeltrace022.py", "ModelConcurrencyTests", FATAL_TEST, FATAL_FIXTURE_BEFORE, FATAL_FIXTURE_AFTER),
        ("tests/test_modeltrace022.py", "ModelConcurrencyTests", CANCEL_TEST, CANCEL_FIXTURE_BEFORE, CANCEL_FIXTURE_AFTER),
        ("tests/test_connection_batch039.py", "ConnectionBatch039Tests", PARENT_SIBLING_TEST, PARENT_SIBLING_FIXTURE_BEFORE, PARENT_SIBLING_FIXTURE_AFTER),
    ):
        _sync_concurrency_method(reference, relative_path, class_name, name, before, after)


def reference_database(postgres: TemporaryPostgres) -> str:
    # The unchanged historical integration tests require this database name
    # and reject URL query overrides. It still belongs to our disposable cluster.
    with psycopg.connect(postgres.url, autocommit=True) as connection:
        connection.execute("CREATE DATABASE sub2ops_qa")
    return urlsplit(postgres.url)._replace(path="/sub2ops_qa", query="", fragment="").geturl()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--filter",
        help="pass a test-name substring to unittest discover -k",
    )
    args = parser.parse_args(argv)
    if args.filter == "":
        parser.error("--filter must not be empty")
    archive = subprocess.run(
        ["git", "archive", REFERENCE_SHA], cwd=ROOT, capture_output=True, check=True
    ).stdout
    with tempfile.TemporaryDirectory(prefix="sub2ops-reference-") as directory:
        reference = Path(directory)
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as bundle:
            bundle.extractall(reference, filter="data")
        apply_concurrency_fixture_sync(reference)
        print(
            "Reference fixture sync override: temporary tests/test_modeltrace022.py and tests/test_connection_batch039.py only; "
            "first-batch, fatal-peer, cancel-entry, and parent-lock event barriers; assertion AST preserved; app files untouched by override."
        )
        env = os.environ.copy()
        for key in tuple(env):
            if any(word in key.upper() for word in ("TOKEN", "SECRET", "PASSWORD", "COOKIE", "API_KEY", "PRIVATE_KEY")):
                env.pop(key, None)
            elif key.startswith(("DATABASE_", "SUB2API_", "OAUTH_", "BARK_", "KEY_FALLBACK_", "USAGE_QUERY_", "AUDIT_")):
                env.pop(key, None)
        env.update(PYTHONDONTWRITEBYTECODE="1", PYTHONPATH=str(reference))
        postgres = TemporaryPostgres(reference)
        try:
            postgres.start()
            env["SUB2OPS_TEST_DATABASE_URL"] = reference_database(postgres)
            test_command = [
                sys.executable, "-m", "unittest", "discover", "-s", "tests", "-q"
            ]
            if args.filter is not None:
                test_command.extend(["-k", args.filter])
            result = subprocess.run(
                test_command,
                cwd=reference, env=env, capture_output=True, text=True,
            )
        finally:
            postgres.close()
        output = (result.stdout + result.stderr).replace(str(reference), "<reference>").replace(str(ROOT), "<repository>")
        output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
        print(output, end="")
        print(f"Reference {REFERENCE_SHA}; exit {result.returncode}")
        return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
