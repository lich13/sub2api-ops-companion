#!/usr/bin/env python3
"""Run the pinned detection lifecycle tests and emit a safe parity fixture.

The reference application and tests are exported from the fixed Git commit into
a private temporary directory. Nothing in the working-tree Python application
is imported as a baseline.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
LEGACY_FILES = (
    "tests/test_detection030.py",
    "tests/test_detection_credits034.py",
    "tests/test_detection_disposition027.py",
    "tests/test_detection_rechecks034.py",
    "tests/test_detection_schedulable037.py",
    "tests/test_model_detection027.py",
)
FIXTURE_PATH = ROOT / "tests/contracts/detection-lifecycle/fixtures.json"


class FixtureError(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FixtureError(message)


def safe_environment() -> dict[str, str]:
    allowed = ("PATH", "SYSTEMROOT", "WINDIR", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL")
    environment = {key: os.environ[key] for key in allowed if key in os.environ}
    for key in list(environment):
        if "proxy" in key.lower():
            environment.pop(key, None)
    environment.update(
        {
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONNOUSERSITE": "1",
            "PYTHONHASHSEED": "0",
        }
    )
    return environment


def export_reference(destination: Path) -> Path:
    archive_path = destination / "reference.tar"
    with archive_path.open("wb") as stream:
        result = subprocess.run(
            ["git", "archive", "--format=tar", REFERENCE_SHA, "app", "tests"],
            cwd=ROOT,
            stdout=stream,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    require(result.returncode == 0, "fixed Python reference archive failed")
    try:
        with tarfile.open(archive_path, mode="r:") as archive:
            members = archive.getmembers()
            for member in members:
                path = PurePosixPath(member.name)
                require(
                    not path.is_absolute() and ".." not in path.parts,
                    "fixed reference archive contains an unsafe path",
                )
                require(not member.issym() and not member.islnk(), "reference symlinks are unsupported")
            archive.extractall(destination, filter="data")
    except (OSError, tarfile.TarError, TypeError) as exc:
        raise FixtureError("fixed Python reference archive could not be extracted") from exc
    finally:
        archive_path.unlink(missing_ok=True)

    app_dir = destination / "app"
    require((app_dir / "model_detection.py").is_file(), "fixed reference model_detection.py is missing")
    for relative in LEGACY_FILES:
        require((destination / relative).is_file(), f"fixed reference test is missing: {relative}")
    return destination


def collected_tests(source: str, relative: str) -> list[dict[str, Any]]:
    tree = ast.parse(source, filename=relative)
    tests: list[dict[str, Any]] = []

    def visit(node: ast.Module | ast.ClassDef, prefix: tuple[str, ...]) -> None:
        for child in node.body:
            if isinstance(child, ast.ClassDef):
                visit(child, (*prefix, child.name))
                continue
            if not isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)) or not child.name.startswith("test_"):
                continue

            assertion_lines: set[int] = set()
            assertion_calls: set[int] = set()
            for item in ast.walk(child):
                if isinstance(item, ast.Assert):
                    assertion_lines.add(item.lineno)
                if (
                    isinstance(item, ast.Call)
                    and isinstance(item.func, ast.Attribute)
                    and item.func.attr.startswith("assert")
                ):
                    assertion_calls.add(item.lineno)
            tests.append(
                {
                    "test_id": "py:" + relative.removesuffix(".py").replace("/", ".") + ":" + ".".join((*prefix, child.name)),
                    "line": child.lineno,
                    "assertion_sites": len(assertion_lines | assertion_calls),
                }
            )

    visit(tree, ())
    return tests


def inventory(reference_root: Path) -> tuple[list[dict[str, Any]], int, int]:
    files: list[dict[str, Any]] = []
    total_tests = 0
    total_assertions = 0
    for relative in LEGACY_FILES:
        raw = (reference_root / relative).read_bytes()
        tests = collected_tests(raw.decode("utf-8"), relative)
        files.append(
            {
                "path": relative,
                "source_sha256": hashlib.sha256(raw).hexdigest(),
                "test_count": len(tests),
                "tests": tests,
            }
        )
        total_tests += len(tests)
        total_assertions += sum(test["assertion_sites"] for test in tests)
    require(total_tests == 80, f"pinned legacy inventory changed: expected 80 tests, found {total_tests}")
    return files, total_tests, total_assertions


def pinned_oracle(reference_root: Path) -> dict[str, Any]:
    dispatch = (reference_root / "app/detection_dispatch.py").read_text(encoding="utf-8")
    dispatch_match = re.search(r"^COOLDOWN_SECONDS\s*=\s*(\d+)\s*$", dispatch, re.MULTILINE)
    require(dispatch_match is not None, "fixed source has no automatic-dispatch cooldown constant")
    cooldown_seconds = int(dispatch_match.group(1))

    tests = "\n".join((reference_root / path).read_text(encoding="utf-8") for path in LEGACY_FILES)
    require(
        "timedelta(seconds=300)" in tests and "timedelta(seconds=200)" in tests,
        "pinned tests no longer express the exact 300-second cooldown boundaries",
    )
    return {
        "automatic_dispatch_cooldown_seconds": cooldown_seconds,
        "marked_recheck_shared_cooldown_seconds": cooldown_seconds,
        "default_interval_minutes": 15,
        "notification_retry_seconds": [5, 30, 120, 600],
        "recovery_retry_seconds": [5],
        "target_model": "gpt-5.6-luna",
        "default_detection_model": "gpt-6-luna",
        "source_anchors": {
            "cooldown": "app/detection_dispatch.py:COOLDOWN_SECONDS",
            "legacy_boundary_test": "tests/test_detection030.py:AutomaticCooldownTests.test_all_automatic_sources_and_models_share_exact_300_second_boundary",
            "legacy_recheck_gate_test": "tests/test_detection_rechecks034.py:RecheckAdmissionTests.test_five_minute_gate_defers_marked_recheck_then_allows_at_exact_boundary",
        },
    }


def run_legacy_suite(reference_root: Path) -> subprocess.CompletedProcess[str]:
    modules = [path.removesuffix(".py").replace("/", ".") for path in LEGACY_FILES]
    return subprocess.run(
        [sys.executable, "-m", "unittest", "-v", *modules],
        cwd=reference_root,
        env=safe_environment(),
        capture_output=True,
        text=True,
        timeout=180,
        check=False,
    )


def safe_failure_detail(process: subprocess.CompletedProcess[str], temp_dir: Path) -> str:
    output = process.stdout + process.stderr
    output = output.replace(str(ROOT), "<repository>").replace(str(temp_dir), "<temporary>")
    output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
    lines = [line for line in output.splitlines() if line.strip()]
    return "\n".join(lines[-30:])


def fixture_document(
    files: list[dict[str, Any]], total_tests: int, total_assertions: int, reference_root: Path
) -> dict[str, Any]:
    app_source = (reference_root / "app/model_detection.py").read_bytes()
    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "legacy_inventory": {
            "file_count": len(files),
            "test_count": total_tests,
            "assertion_sites": total_assertions,
            "files": files,
        },
        "reference_sources": {
            "app/model_detection.py": hashlib.sha256(app_source).hexdigest(),
        },
        "oracle": pinned_oracle(reference_root),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--write", action="store_true", help="write the deterministic oracle JSON")
    action.add_argument("--check", action="store_true", help="compare the deterministic oracle JSON")
    args = parser.parse_args(argv)

    require((ROOT / ".venv/bin/python").exists(), "repository Python virtualenv is required")
    try:
        with tempfile.TemporaryDirectory(prefix="sub2ops-detection040-reference-") as temporary:
            temp_dir = Path(temporary)
            reference_root = export_reference(temp_dir)
            files, total_tests, total_assertions = inventory(reference_root)
            baseline = run_legacy_suite(reference_root)
            if baseline.returncode != 0:
                print(safe_failure_detail(baseline, temp_dir), file=sys.stderr)
                return baseline.returncode or 1
            require(
                re.search(r"Ran 80 tests? in ", baseline.stderr) is not None,
                "fixed test runner did not report exactly 80 executed tests",
            )
            require("OK" in baseline.stderr.splitlines()[-1:], "fixed reference unittest suite did not pass")
            document = fixture_document(files, total_tests, total_assertions, reference_root)

        rendered = json.dumps(document, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
        if args.write:
            FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
            FIXTURE_PATH.write_text(rendered, encoding="utf-8")
        elif args.check:
            require(FIXTURE_PATH.is_file(), "detection lifecycle fixture JSON is missing")
            require(FIXTURE_PATH.read_text(encoding="utf-8") == rendered, "detection lifecycle fixture JSON is stale")
        elif not FIXTURE_PATH.is_file():
            raise FixtureError("detection lifecycle fixture JSON is missing; rerun with --write")

        print(
            json.dumps(
                {
                    "reference_sha": REFERENCE_SHA,
                    "test_count": total_tests,
                    "assertion_sites": total_assertions,
                    "baseline_exit_code": baseline.returncode,
                    "fixture_sha256": hashlib.sha256(rendered.encode("utf-8")).hexdigest(),
                    "fixture_action": "written" if args.write else "checked" if args.check else "verified",
                },
                sort_keys=True,
            )
        )
        return 0
    except (FixtureError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as exc:
        print(f"detection fixture failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
