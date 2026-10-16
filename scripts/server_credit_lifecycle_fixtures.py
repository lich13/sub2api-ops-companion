#!/usr/bin/env python3
"""Rebuild the fixed-SHA legacy oracle for the Rust credit lifecycle parity tests.

Run from the repository with the project Python environment:
  .venv/bin/python scripts/server_credit_lifecycle_fixtures.py
  .venv/bin/python scripts/server_credit_lifecycle_fixtures.py --check

The selected Python tests are extracted from one pinned Git archive, run with
an empty HOME and loopback-only networking, and discarded with the archive.
Only derived JSON outputs and source hashes are retained in the repository.
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
from io import BytesIO
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
FIXTURE_PATH = ROOT / "tests" / "contracts" / "credit-lifecycle" / "fixtures.json"
SPECS = (
    ("tests/test_auto_reset.py", ("AutoResetEligibilityTests", "AutoResetLifecycleTests", "AutoResetHttpBudgetTests", "ResetCreditDiagnosticTests")),
    ("tests/test_auto_reset037.py", ("AutoReset037BoundedWindowTests", "AutoReset037NotificationTests")),
    ("tests/test_auto_reset037_boundaries.py", None),
    ("tests/test_auto_reset038.py", ("AutoReset038AdmissionTests", "AutoReset038ObservationTests")),
    ("tests/test_credit_manual037.py", ("ManualCredit037BoundaryTests",)),
    ("tests/test_credit_degradation037.py", ("CreditDegradationMerge037Tests",)),
    ("tests/test_credit_results_http037.py", ("CreditResultHttp037Tests",)),
    ("tests/test_recovery019.py", ("Recovery019AutoResetTests",)),
)
EXCLUDED = (
    "tests.test_recovery019.Recovery019MonitorFixture.test_passive_quota_crossing_reset_point_is_reusable",
    "tests.test_recovery019.Recovery019MonitorFixture.test_new_rate_limit_after_recovered_intent_retests_from_passive_quota",
    "tests.test_recovery019.Recovery019MonitorFixture.test_failed_test_retries_after_one_minute_without_quota_cooldown_or_budget",
    "tests.test_recovery019.Recovery019MonitorFixture.test_manual_unschedulable_account_is_not_tested_or_recovered",
    "tests.test_recovery019.Recovery019MonitorFixture.test_new_block_or_manual_change_during_test_is_never_cleared",
)
ORACLE_SOURCE_PATHS = tuple(path for path, _ in SPECS) + (
    "app/auto_reset.py",
    "app/reset_credit_observation.py",
)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def run(command: list[str], *, cwd: Path, env: dict[str, str], timeout: int = 300) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def test_ids(root: Path) -> list[str]:
    selected: list[str] = []
    for relative, classes in SPECS:
        path = root / relative
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=relative)
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            if classes is not None and node.name not in classes:
                continue
            for item in node.body:
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)) and item.name.startswith("test_"):
                    selected.append(f"{relative[:-3].replace('/', '.')}.{node.name}.{item.name}")
    if len(selected) != 100 or len(set(selected)) != 100:
        raise RuntimeError(f"fixed reference inventory changed: expected 100 unique tests, found {len(selected)}")
    return selected


def source_hashes(root: Path) -> dict[str, str]:
    return {relative: digest((root / relative).read_bytes()) for relative in ORACLE_SOURCE_PATHS}


def safe_environment(temporary: Path, archive_root: Path, firewall_dir: Path) -> dict[str, str]:
    home = temporary / "empty-home"
    scratch = temporary / "scratch"
    home.mkdir(mode=0o700)
    scratch.mkdir(mode=0o700)
    allowed = {name: os.environ[name] for name in ("PATH", "LANG", "LC_ALL", "LC_CTYPE") if name in os.environ}
    allowed.update(
        {
            "HOME": str(home),
            "TMPDIR": str(scratch),
            "TMP": str(scratch),
            "TEMP": str(scratch),
            "PYTHONPATH": os.pathsep.join((str(firewall_dir), str(archive_root))),
            "PYTHONNOUSERSITE": "1",
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONUTF8": "1",
            "PYTHONHASHSEED": "0",
            "TZ": "UTC",
            "NO_PROXY": "127.0.0.1,localhost,::1",
            "no_proxy": "127.0.0.1,localhost,::1",
        }
    )
    return allowed


def firewall_source() -> str:
    return '''\
import ipaddress
import socket

_connect = socket.socket.connect
_connect_ex = socket.socket.connect_ex
_sendto = socket.socket.sendto
_getaddrinfo = socket.getaddrinfo
_gethostbyname = socket.gethostbyname
_gethostbyname_ex = socket.gethostbyname_ex

def _loopback(host):
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host.split("%", 1)[0]).is_loopback
    except (ValueError, AttributeError, TypeError):
        return False

def _allow_address(address):
    return isinstance(address, tuple) and bool(address) and _loopback(address[0])

def _guard_connect(sock, address):
    if not _allow_address(address):
        raise AssertionError("fixed-SHA legacy oracle forbids non-loopback networking")
    return _connect(sock, address)

def _guard_connect_ex(sock, address):
    if not _allow_address(address):
        raise AssertionError("fixed-SHA legacy oracle forbids non-loopback networking")
    return _connect_ex(sock, address)

def _guard_sendto(sock, data, *args):
    address = args[-1] if args else None
    if not _allow_address(address):
        raise AssertionError("fixed-SHA legacy oracle forbids non-loopback networking")
    return _sendto(sock, data, *args)

def _guard_getaddrinfo(host, *args, **kwargs):
    if not _loopback(host):
        raise AssertionError("fixed-SHA legacy oracle forbids external DNS")
    return _getaddrinfo(host, *args, **kwargs)

def _guard_gethostbyname(host):
    if not _loopback(host):
        raise AssertionError("fixed-SHA legacy oracle forbids external DNS")
    return _gethostbyname(host)

def _guard_gethostbyname_ex(host):
    if not _loopback(host):
        raise AssertionError("fixed-SHA legacy oracle forbids external DNS")
    return _gethostbyname_ex(host)

socket.socket.connect = _guard_connect
socket.socket.connect_ex = _guard_connect_ex
socket.socket.sendto = _guard_sendto
socket.getaddrinfo = _guard_getaddrinfo
socket.gethostbyname = _guard_gethostbyname
socket.gethostbyname_ex = _guard_gethostbyname_ex
'''


def parse_test_run(output: str, selected: list[str]) -> dict[str, Any]:
    executed = re.findall(r"^\s*(test_[A-Za-z0-9_]+) \((tests\.[^)]+)\) \.\.\. (ok|FAIL|ERROR)$", output, re.M)
    passed = sorted(f"{module}.{method}" for method, module, status in executed if status == "ok")
    failed = sorted(f"{module}.{method}" for method, module, status in executed if status != "ok")
    summary = re.search(r"^Ran (\d+) tests? in [^\n]+", output, re.M)
    return {
        "status": "passed" if len(passed) == len(selected) and not failed and summary else "failed",
        "selected": len(selected),
        "passed": len(passed),
        "failed": len(failed),
        "passed_ids": passed,
        "failed_ids": failed,
        "reported_count": int(summary.group(1)) if summary else None,
    }


def recovery019_oracle(archive_root: Path, env: dict[str, str]) -> dict[str, Any]:
    code = r'''
import copy, json
from datetime import timedelta
from tests.test_auto_reset import AutoResetFixture
from tests.test_recovery019 import NOW
from app.auto_reset import quota_result

fixture = AutoResetFixture("runTest")
fixture.setUp()
try:
    row = copy.deepcopy(fixture.row)
    row["credentials"] = {"plan_type": "free"}
    reset_at = NOW + timedelta(days=30)
    data = {
        "fetched_at": NOW.isoformat(),
        "rate_limit": {
            "primary_window": {
                "limit_window_seconds": 2592000,
                "used_percent": 20,
                "reset_at": int(reset_at.timestamp()),
                "reset_after_seconds": 2592000,
            },
        },
    }
    result = quota_result(data, row, NOW)
    safe_row = {"id": row["id"], "credentials": {"plan_type": "free"}, "extra": row["extra"]}
    print(json.dumps({"input": {"at": NOW.isoformat(), "data": data, "row": safe_row,
                           "expected_reset_at": reset_at.isoformat()},
                      "expected": result}, sort_keys=True, separators=(",", ":")))
finally:
    fixture.doCleanups()
'''
    completed = run([sys.executable, "-c", code], cwd=archive_root, env=env, timeout=30)
    if completed.returncode != 0:
        raise RuntimeError("legacy quota_result oracle process failed")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError("legacy quota_result oracle did not emit JSON") from exc


def build_fixture() -> tuple[dict[str, Any] | None, str]:
    commit = run(["git", "rev-parse", "--verify", REFERENCE_SHA + "^{commit}"], cwd=ROOT, env=os.environ.copy())
    if commit.returncode != 0 or commit.stdout.strip() != REFERENCE_SHA:
        return None, "fixed legacy commit is unavailable"
    archive = subprocess.run(
        ["git", "archive", "--format=tar", REFERENCE_SHA, "app", "tests"],
        cwd=ROOT,
        capture_output=True,
        check=False,
    )
    if archive.returncode != 0:
        return None, "fixed legacy archive could not be read"

    try:
        with tempfile.TemporaryDirectory(prefix="server-credit-lifecycle-oracle-") as directory:
            temporary = Path(directory)
            archive_root = temporary / "pinned-source"
            archive_root.mkdir(mode=0o700)
            with tarfile.open(fileobj=BytesIO(archive.stdout), mode="r:") as tar:
                for member in tar.getmembers():
                    member_path = Path(member.name)
                    if member_path.is_absolute() or ".." in member_path.parts:
                        return None, "fixed Git archive contains an unsafe path"
                tar.extractall(archive_root, filter="data")

            selected = test_ids(archive_root)
            for excluded in EXCLUDED:
                if excluded in selected:
                    return None, "Recovery019 monitor fixture leaked into the selected legacy scope"
            firewall = temporary / "firewall"
            firewall.mkdir(mode=0o700)
            (firewall / "sitecustomize.py").write_text(firewall_source(), encoding="utf-8")
            env = safe_environment(temporary, archive_root, firewall)
            completed = run([sys.executable, "-m", "unittest", "-v", *selected], cwd=archive_root, env=env)
            legacy_run = parse_test_run(completed.stderr + completed.stdout, selected)
            if completed.returncode != 0 or legacy_run["status"] != "passed":
                failed = "\n".join(legacy_run["failed_ids"][:20]) or "unparsed legacy unittest failure"
                return None, f"fixed-SHA legacy tests failed ({legacy_run['passed']}/100 passed): {failed}"
            result = {
                "reference_sha": REFERENCE_SHA,
                "oracle_source_sha256": source_hashes(archive_root),
                "legacy_run": legacy_run,
                "legacy_test_ids": selected,
                "excluded_legacy_tests": [
                    {"legacy_test_id": legacy_id, "reason": "outside delegated scope: Recovery019MonitorFixture"}
                    for legacy_id in EXCLUDED
                ],
                "oracle_outputs": {
                    "recovery019_free_primary_thirty_day": recovery019_oracle(archive_root, env),
                },
            }
            return result, ""
    except (OSError, subprocess.SubprocessError, tarfile.TarError, RuntimeError) as exc:
        return None, f"fixed-SHA oracle preparation failed: {type(exc).__name__}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify the checked-in fixture without rewriting it")
    args = parser.parse_args()
    result, error = build_fixture()
    if error:
        print(error, file=sys.stderr)
        return 1
    encoded = json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if args.check:
        if not FIXTURE_PATH.is_file() or FIXTURE_PATH.read_text(encoding="utf-8") != encoded:
            print("credit lifecycle fixture differs from the pinned-SHA oracle", file=sys.stderr)
            return 1
        print("fixed-SHA oracle verified: 100/100 legacy tests, no external requests")
        return 0
    FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
    FIXTURE_PATH.write_text(encoded, encoding="utf-8")
    print("fixed-SHA oracle generated: 100/100 legacy tests, no external requests")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
