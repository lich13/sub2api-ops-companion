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


def _concurrency_method(source: str) -> ast.AsyncFunctionDef:
    tree = ast.parse(source)
    matches = [
        method
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == "ModelConcurrencyTests"
        for method in node.body
        if isinstance(method, ast.AsyncFunctionDef) and method.name == CONCURRENCY_TEST
    ]
    if len(matches) != 1:
        raise RuntimeError(f"Expected one {CONCURRENCY_TEST} fixture; found {len(matches)}")
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
    relative_path = Path("tests/test_modeltrace022.py")
    test_path = reference / relative_path
    source = test_path.read_bytes().decode("utf-8")
    method = _concurrency_method(source)
    if method.end_lineno is None:
        raise RuntimeError(f"Cannot locate the end of {CONCURRENCY_TEST}")
    lines = source.splitlines(keepends=True)
    method_start, method_end = method.lineno - 1, method.end_lineno
    method_source = "".join(lines[method_start:method_end])
    matches = method_source.count(CONCURRENCY_FIXTURE_BEFORE)
    if matches != 1:
        raise RuntimeError(
            f"Expected one historical concurrency fixture in {relative_path}; found {matches}"
        )

    patched_method = method_source.replace(
        CONCURRENCY_FIXTURE_BEFORE, CONCURRENCY_FIXTURE_AFTER, 1
    )
    patched_source = (
        "".join(lines[:method_start])
        + patched_method
        + "".join(lines[method_end:])
    )
    original_assertions = _self_assertions(method)
    patched_assertions = _self_assertions(_concurrency_method(patched_source))
    if original_assertions != patched_assertions:
        raise RuntimeError(f"{CONCURRENCY_TEST} assertion AST changed during fixture sync")
    test_path.write_bytes(patched_source.encode("utf-8"))


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
            "Reference fixture sync override: temporary tests/test_modeltrace022.py only; "
            "first-batch event barrier; assertion AST preserved; app files untouched by override."
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
