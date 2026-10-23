#!/usr/bin/env python3
"""Run pinned OAuth scheduler tests and the real Rust runtime contract."""
from __future__ import annotations

import argparse
import ast
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts"))

from server_credit_contract import safe_environment  # noqa: E402
from server_http_contract import TemporaryPostgres  # noqa: E402

FIXTURE_PATH = ROOT / "tests" / "contracts" / "oauth-scheduler" / "fixtures.json"
MIGRATION_PATH = ROOT / "tests" / "contracts" / "oauth-scheduler" / "migration-fragment.json"
TEST_TARGET = "oauth_scheduler_parity"
from server_reference import REFERENCE_SHA

if json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))["reference_sha"] != REFERENCE_SHA:
    raise RuntimeError("fixture reference differs from pinned Python revision")
REFERENCE_PATHS = (
    "app",
    "tests/test_oauth_queries.py",
    "tests/test_daily_test.py",
    "tests/test_oauth_monitor.py",
    "tests/test_usage_query.py",
    "tests/test_recovery_selection034.py",
)
LEGACY_SCOPE = {
    "tests/test_oauth_queries.py": {
        "classes": ("OAuthQueryCoordinatorTests", "OAuthQueryLinkageTests"),
    },
    "tests/test_daily_test.py": {
        "classes": ("DailyScheduleTests", "DailyExecutionTests"),
    },
    "tests/test_oauth_monitor.py": {
        "classes": (
            "OAuthStateStoreTests",
            "OAuthRecoveryHttpTests",
            "OAuthMonitorExecutionTests",
        ),
    },
    "tests/test_usage_query.py": {
        "methods": {
            "OAuthUsageTests": ("test_active_usage_uses_x_api_key_without_bearer_header",),
        },
    },
    "tests/test_recovery_selection034.py": {
        "methods": {
            "RecoverySelection034Tests": (
                "test_monitor_run_once_refreshes_inventory_without_network_when_recovery_is_disabled",
            ),
        },
    },
}
EXPECTED_COUNTS = {
    "oauth_queries": 39,
    "daily_test": 13,
    "oauth_monitor_runtime_http_state": 32,
    "usage_query": 1,
    "recovery_selection_runtime_extra": 1,
    "excluded_monitor": 8,
    "previously_mapped_oauth_queries": 2,
    "base_total": 85,
    "total": 86,
}


class ContractFailure(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractFailure(message)


def sanitized(output: str, temporary: str) -> str:
    return output.replace(str(ROOT), "<repository>").replace(temporary, "<temporary>")


def archive_reference(destination: Path) -> None:
    destination.mkdir(mode=0o700, parents=True, exist_ok=True)
    archive_path = destination.parent / "oauth-scheduler-reference.tar"
    with archive_path.open("wb") as archive_file:
        result = subprocess.run(
            ["git", "archive", "--format=tar", REFERENCE_SHA, *REFERENCE_PATHS],
            cwd=ROOT,
            stdout=archive_file,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    require(result.returncode == 0, "fixed Python OAuth scheduler reference could not be archived")
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
        raise ContractFailure("fixed Python OAuth scheduler reference could not be extracted") from exc
    finally:
        archive_path.unlink(missing_ok=True)
    for path in REFERENCE_PATHS[1:]:
        require((destination / path).is_file(), f"pinned test source is missing: {path}")
    (destination / "tests" / "__init__.py").touch(mode=0o600)


def selected_test_ids(reference: Path) -> tuple[list[str], dict[str, int]]:
    selected: list[str] = []
    counts: dict[str, int] = {}
    for path, scope in LEGACY_SCOPE.items():
        source = (reference / path).read_text(encoding="utf-8")
        tree = ast.parse(source, filename=path)
        module = path.removeprefix("tests/").removesuffix(".py").replace("/", ".")
        classes = set(scope.get("classes", ()))
        methods = scope.get("methods", {})
        module_count = 0
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            explicit = set(methods.get(node.name, ()))
            if node.name not in classes and not explicit:
                continue
            for member in node.body:
                if not isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    continue
                if not member.name.startswith("test_"):
                    continue
                if explicit and member.name not in explicit:
                    continue
                selected.append(f"tests.{module}.{node.name}.{member.name}")
                module_count += 1
        counts[path] = module_count

    expected_module_counts = {
        "tests/test_oauth_queries.py": 39,
        "tests/test_daily_test.py": 13,
        "tests/test_oauth_monitor.py": 32,
        "tests/test_usage_query.py": 1,
        "tests/test_recovery_selection034.py": 1,
    }
    require(counts == expected_module_counts, f"pinned selected-test scope changed: {counts}")
    excluded_tree = ast.parse(
        (reference / "tests/test_oauth_monitor.py").read_text(encoding="utf-8"),
        filename="tests/test_oauth_monitor.py",
    )
    excluded = [
        f"tests.test_oauth_monitor.{node.name}.{member.name}"
        for node in excluded_tree.body
        if isinstance(node, ast.ClassDef) and node.name in {"OAuthMonitorSchedulingTests", "OAuthRecoveryGateTests"}
        for member in node.body
        if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)) and member.name.startswith("test_")
    ]
    require(len(excluded) == EXPECTED_COUNTS["excluded_monitor"], "pinned monitor exclusion count changed")
    require(not set(excluded).intersection(selected), "excluded monitor rules leaked into this runtime block")
    require(len(selected) == EXPECTED_COUNTS["total"], "pinned selected-test total changed")
    return selected, counts


def legacy_id(test_id: str) -> str:
    module, class_name, method = test_id.rsplit(".", 2)
    return f"py:{module}:{class_name}.{method}"


def json_path_exists(document: object, path: str) -> bool:
    current = document
    for part in path.split("."):
        if isinstance(current, dict) and part in current:
            current = current[part]
        elif isinstance(current, list) and part.isdecimal() and int(part) < len(current):
            current = current[int(part)]
        else:
            return False
    return True


def rust_test_section(source: str, name: str) -> str | None:
    match = re.search(rf"(?m)^(?:async\s+)?fn\s+{re.escape(name)}\s*\(", source)
    if match is None:
        return None
    following = re.search(r"(?m)^#\[(?:tokio::test|test)\]\s*$", source[match.end() :])
    end = len(source) if following is None else match.end() + following.start()
    return source[match.start() : end]


def validate_migration_fragment(selected: list[str]) -> None:
    try:
        fragment = json.loads(MIGRATION_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ContractFailure("OAuth scheduler migration fragment is missing or invalid JSON") from exc
    require(fragment.get("schema_version") == 1, "OAuth scheduler mapping schema version changed")
    require(fragment.get("reference_sha") == REFERENCE_SHA, "OAuth scheduler mapping reference differs")
    mappings = fragment.get("mappings")
    require(isinstance(mappings, list), "OAuth scheduler migration mappings must be a list")
    by_id: dict[str, dict[str, object]] = {}
    for mapping in mappings:
        require(isinstance(mapping, dict), "OAuth scheduler mapping row must be an object")
        identity = mapping.get("legacy_test_id")
        require(isinstance(identity, str) and identity not in by_id, "OAuth scheduler mapping IDs must be unique")
        by_id[identity] = mapping
    expected_ids = {legacy_id(test_id) for test_id in selected}
    require(
        set(by_id) == expected_ids,
        "OAuth scheduler migration mapping IDs do not exactly match the pinned selectors",
    )

    for identity, mapping in by_id.items():
        require(
            mapping.get("status") in {"mapped", "partial", "superseded"},
            f"OAuth scheduler mapping status is invalid: {identity}",
        )
        require(bool(mapping.get("coverage_note")), f"OAuth scheduler mapping has no coverage note: {identity}")
        if mapping["status"] == "partial":
            require(bool(mapping.get("uncovered_assertions")), f"partial mapping has no gap: {identity}")
        if mapping["status"] == "superseded":
            require(
                bool(mapping.get("superseded_by")) and bool(mapping.get("approved_behavior")),
                f"superseded mapping needs an approved replacement: {identity}",
            )
        target = mapping.get("target")
        require(isinstance(target, dict) and target.get("kind") == "rust_test", f"mapping target is invalid: {identity}")
        relative = target.get("path")
        name = target.get("test_name")
        require(isinstance(relative, str) and isinstance(name, str), f"mapping target fields are missing: {identity}")
        target_path = (ROOT / relative).resolve()
        require(target_path.is_relative_to(ROOT) and target_path.is_file(), f"mapping target file is missing: {identity}")
        source = target_path.read_text(encoding="utf-8")
        test_section = rust_test_section(source, name)
        require(test_section is not None, f"mapping Rust test does not exist: {identity}")
        assertions = target.get("assertions")
        require(isinstance(assertions, list) and assertions, f"mapping target assertions are empty: {identity}")
        normalized_section = re.sub(r"\s+", "", test_section)
        for assertion in assertions:
            require(
                isinstance(assertion, str)
                and re.sub(r"\s+", "", assertion) in normalized_section,
                f"mapping assertion is not present in the target Rust test: {identity}",
            )
        fixtures = target.get("fixtures", [])
        require(isinstance(fixtures, list), f"mapping fixtures must be a list: {identity}")
        for fixture in fixtures:
            require(isinstance(fixture, dict), f"mapping fixture must be an object: {identity}")
            fixture_relative = fixture.get("path")
            paths = fixture.get("json_paths", [])
            require(isinstance(fixture_relative, str) and isinstance(paths, list), f"mapping fixture fields are invalid: {identity}")
            fixture_path = (ROOT / fixture_relative).resolve()
            require(fixture_path.is_relative_to(ROOT) and fixture_path.is_file(), f"mapping fixture is missing: {identity}")
            if paths:
                try:
                    document = json.loads(fixture_path.read_text(encoding="utf-8"))
                except json.JSONDecodeError as exc:
                    raise ContractFailure(f"mapping fixture JSON is invalid: {identity}") from exc
                require(
                    all(isinstance(path, str) and json_path_exists(document, path) for path in paths),
                    f"mapping fixture JSON path is missing: {identity}",
                )


def blocked_network_sitecustomize(path: Path) -> None:
    path.write_text(
        """import ipaddress\nimport socket\n\n_original_getaddrinfo = socket.getaddrinfo\n_original_connect = socket.socket.connect\n_original_connect_ex = socket.socket.connect_ex\n\ndef _loopback(host):\n    if host in (\"localhost\", \"127.0.0.1\", \"::1\"):\n        return True\n    try:\n        return ipaddress.ip_address(host).is_loopback\n    except (ValueError, TypeError):\n        return False\n\ndef _getaddrinfo(host, *args, **kwargs):\n    if not _loopback(host):\n        raise socket.gaierror(\"OAuth scheduler fixture blocks non-loopback DNS\")\n    return _original_getaddrinfo(host, *args, **kwargs)\n\ndef _connect(sock, address):\n    if not isinstance(address, tuple) or not _loopback(address[0]):\n        raise OSError(\"OAuth scheduler fixture blocks non-loopback sockets\")\n    return _original_connect(sock, address)\n\ndef _connect_ex(sock, address):\n    if not isinstance(address, tuple) or not _loopback(address[0]):\n        raise OSError(\"OAuth scheduler fixture blocks non-loopback sockets\")\n    return _original_connect_ex(sock, address)\n\nsocket.getaddrinfo = _getaddrinfo\nsocket.socket.connect = _connect\nsocket.socket.connect_ex = _connect_ex\n""",
        encoding="utf-8",
    )
    path.chmod(0o600)


def legacy_environment(temporary: Path, reference: Path, database_url: str) -> dict[str, str]:
    env = safe_environment(database_url, temporary)
    home = temporary / "home"
    home.mkdir(mode=0o700, exist_ok=True)
    guard = temporary / "network_guard"
    guard.mkdir(mode=0o700, exist_ok=True)
    blocked_network_sitecustomize(guard / "sitecustomize.py")
    env.update(
        {
            "HOME": str(home),
            "PYTHONPATH": os.pathsep.join((str(guard), str(reference), str(reference / "tests"))),
            "PYTHONDONTWRITEBYTECODE": "1",
            "NO_PROXY": "127.0.0.1,localhost,::1",
            "no_proxy": "127.0.0.1,localhost,::1",
        }
    )
    return env


def run(
    command: list[str], *, cwd: Path, env: dict[str, str], temporary: str,
    label: str, timeout_seconds: int,
) -> int:
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
        print(f"{label}: exit 124")
        return 124
    output = sanitized(result.stdout + result.stderr, temporary)
    if output:
        print(output, end="" if output.endswith("\n") else "\n")
    print(f"{label}: exit {result.returncode}")
    return result.returncode


def run_legacy(reference: Path, temporary: Path) -> int:
    selected, counts = selected_test_ids(reference)
    validate_migration_fragment(selected)
    print(f"pinned reference: {REFERENCE_SHA}")
    print(f"selected legacy tests: {len(selected)}; per-file counts: {json.dumps(counts, sort_keys=True)}")
    env = legacy_environment(
        temporary,
        reference,
        "postgresql://fixture:fixture@127.0.0.1:1/fixture-disabled",
    )
    return run(
        [sys.executable, "-m", "unittest", *selected],
        cwd=reference,
        env=env,
        temporary=str(temporary),
        label="pinned Python OAuth scheduler selectors",
        timeout_seconds=180,
    )


def run_rust(
    temporary: Path,
    postgres_bin_dir: str | None,
    timeout_seconds: int,
    rust_filters: list[str],
) -> int:
    postgres = TemporaryPostgres(temporary, postgres_bin_dir)
    try:
        postgres.start()
        env = safe_environment(postgres.url, temporary)
        command = [
            "cargo",
            "test",
            "--locked",
            "--offline",
            "--manifest-path",
            "server/Cargo.toml",
            "--test",
            TEST_TARGET,
        ]
        filters: list[str | None] = rust_filters or [None]
        first_failure = 0
        for rust_filter in filters:
            filtered_command = command.copy()
            if rust_filter:
                filtered_command.append(rust_filter)
            filtered_command.extend(("--", "--test-threads=1"))
            label = "Rust OAuth scheduler runtime contract"
            if rust_filter:
                label = f"{label} [{rust_filter}]"
            result = run(
                filtered_command,
                cwd=ROOT,
                env=env,
                temporary=str(temporary),
                label=label,
                timeout_seconds=timeout_seconds,
            )
            if result != 0 and first_failure == 0:
                first_failure = result
        return first_failure
    finally:
        postgres.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-bin-dir")
    parser.add_argument("--legacy-only", action="store_true", help="run the archived Python selectors only")
    parser.add_argument("--rust-only", action="store_true", help="run the Rust integration target only")
    parser.add_argument(
        "--rust-filter",
        action="append",
        default=[],
        help="optional Rust test-name substring; repeat to run several filters in one temporary database",
    )
    parser.add_argument("--timeout", type=int, default=600, help="Rust test timeout in seconds")
    args = parser.parse_args()
    if args.legacy_only and args.rust_only:
        parser.error("--legacy-only and --rust-only are mutually exclusive")

    exit_code = 0
    with tempfile.TemporaryDirectory(prefix="sub2ops-oauth-scheduler-") as directory:
        temporary = Path(directory)
        reference = temporary / "reference"
        if not args.rust_only:
            archive_reference(reference)
            exit_code = run_legacy(reference, temporary)
        if exit_code == 0 and not args.legacy_only:
            exit_code = run_rust(temporary, args.postgres_bin_dir, args.timeout, args.rust_filter)
    print(f"temporary fixture cleanup: completed; exit {exit_code}")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
