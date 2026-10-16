#!/usr/bin/env python3
"""Run the Rust state/lifecycle contract against pinned Python and owned PostgreSQL."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
FIXTURE_PATH = ROOT / "tests" / "contracts" / "state" / "fixtures.json"
sys.path.insert(0, str(ROOT / "scripts"))
import server_http_contract as http_contract


class ContractFailure(RuntimeError):
    pass


def require(value: bool, message: str) -> None:
    if not value:
        raise ContractFailure(message)


def progress(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def safe_environment(state_root: Path, database_url: str, output_root: Path) -> dict[str, str]:
    safe = os.environ.copy()
    for name in tuple(safe):
        upper = name.upper()
        if any(fragment in upper for fragment in (
            "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "API_KEY", "APIKEY",
            "PRIVATE_KEY", "PRIVATEKEY", "COOKIE", "ACCESS_KEY", "ACCESSKEY",
        )) or upper.startswith(("AWS_", "AZURE_", "GOOGLE_", "GITHUB_", "OPENAI_", "ANTHROPIC_", "SENTRY_")):
            safe.pop(name, None)
    for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
        safe.pop(name, None)

    temporary = state_root / "tmp"
    temporary.mkdir(mode=0o700, parents=True, exist_ok=True)
    safe.update({
        "DATABASE_URL": database_url,
        "STATE_LIFECYCLE_CONTRACT_DIR": str(output_root),
        "RUST_TEST_THREADS": "1",
        "CARGO_NET_OFFLINE": "true",
        "TMPDIR": str(temporary),
        "TMP": str(temporary),
        "TEMP": str(temporary),
        "NO_PROXY": "127.0.0.1,localhost,::1",
        "no_proxy": "127.0.0.1,localhost,::1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONUNBUFFERED": "1",
    })
    return safe


def command_for(all_rust: bool) -> list[str]:
    base = ["cargo", "test", "--locked", "--offline", "--manifest-path", "server/Cargo.toml"]
    if all_rust:
        return [*base, "--", "--test-threads=1"]
    return [*base, "--test", "state_lifecycle_contract", "--", "--test-threads=1", "--nocapture"]


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sanitized_tail(output: str, temporary: Path, database_url: str) -> str:
    value = output.replace(str(temporary), "<runner-temporary>")
    value = value.replace(database_url, "<runner-owned-loopback-postgres>")
    value = re.sub(r"postgres(?:ql)?://[^\s'\"]+", "<postgres-url>", value)
    return "\n".join(value.splitlines()[-60:])


def verify_numeric_reference(postgres_url: str, reference_root: Path, fixture: dict[str, Any]) -> dict[str, Any]:
    sys.path.insert(0, str(reference_root))
    from app.operation_versions import digest

    values = fixture["numeric"]["amounts"]
    expressions: list[str] = []
    for index, item in enumerate(values):
        literal = item["sql_literal"]
        require(bool(re.fullmatch(r"[0-9+Ee.-]+", literal)), "numeric fixture contains a nonnumeric SQL literal")
        expressions.append(f"'{literal}'::numeric AS amount_{index}")
    import psycopg

    with psycopg.connect(postgres_url) as connection:
        row = connection.execute("SELECT " + ", ".join(expressions)).fetchone()
    actual = [str(value) for value in row]
    expected = [item["expected"] for item in values]
    require(actual == expected, "psycopg Decimal string values differ from pinned NUMERIC vectors")
    serialized = json.dumps({"amounts": list(row)}, sort_keys=True, default=str, ensure_ascii=False)
    require(serialized == fixture["numeric"]["python_default_str_serialization"],
            "psycopg Decimal/default=str serialization differs from the fixed fixture")
    actual_digest = digest({"amounts": list(row)})
    require(actual_digest == fixture["numeric"]["python_operation_digest"],
            "pinned Python operation digest differs from the fixed numeric fixture")
    return {"values": actual, "python_operation_digest": actual_digest}


def verify_python_readback(reference_root: Path, output_root: Path,
                           fixture: dict[str, Any], numeric: dict[str, Any]) -> list[str]:
    sys.path.insert(0, str(reference_root))
    from app.oauth_monitor import OAuthStateStore
    from app.policy_store import PolicyStore

    checks: list[str] = ["Python Decimal dscale and legacy digest"]
    cross = output_root / "cross-language"
    rust_state = PolicyStore(cross / "rust-state.json", {}).read()
    require(rust_state == fixture["cross_language"]["rust_output"],
            "pinned Python PolicyStore could not read Rust-written state")
    checks.append("pinned Python 6be reads Rust-written PolicyStore state")

    runtime_root = output_root / "runtime-migration"
    oauth = OAuthStateStore(str(runtime_root / "usage-query-state.json")).snapshot()
    require(oauth["settings"].get("legacy_recovery_cleanup_completed") is True,
            "pinned Python OAuthStateStore could not read the Rust migration marker")
    require(bool(oauth["settings"].get("legacy_recovery_cleanup_completed_at")),
            "Rust migration timestamp is absent from Python OAuthStateStore readback")

    profiles = PolicyStore(runtime_root / "account-model-profiles.json", {}).read()
    expected_statuses = fixture["runtime"]["expected_statuses"]
    profile_items = profiles["jobs"]["fixture-job"]["items"]
    for item in profile_items:
        item_id = item["id"]
        require(item.get("status") == expected_statuses[item_id],
                f"pinned Python sees unexpected legacy profile status for {item_id}")
    capacity = json.loads((runtime_root / "capacity-alert-state.json").read_text(encoding="utf-8"))
    for item_id, item in capacity["profile_intents"].items():
        require(item.get("status") == expected_statuses[item_id],
                f"pinned Python sees unexpected capacity intent status for {item_id}")
    config = json.loads((runtime_root / "oauth-config.json").read_text(encoding="utf-8"))
    require(set(config) == set(fixture["runtime"]["legacy_config_retained_keys"]),
            "old OAuth config cleanup retained or removed the wrong keys")
    require(not (runtime_root / "guard-state.json").exists(), "legacy guard state was not retired")

    new_capacity = json.loads(
        (output_root / "runtime-new-directory" / "capacity-alert-state.json").read_text(encoding="utf-8")
    )
    require("profile_intents" not in new_capacity,
            "fresh Runtime startup inserted a missing profile_intents field")
    checks.extend([
        "pinned Python OAuthStateStore reads the recovery cleanup marker",
        "pinned Python PolicyStore reads retired profile jobs",
        "pinned Python reads retired capacity intents and filtered legacy config",
        "fresh writable Runtime starts with absent profile_intents",
        "legacy guard file is absent after startup",
    ])
    return checks


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-bin-dir", help="directory containing initdb and pg_ctl")
    parser.add_argument("--cargo", default="cargo", help="Cargo executable (default: cargo)")
    parser.add_argument("--all-rust", action="store_true",
                        help="run all Rust unit and integration tests in the same temporary PostgreSQL session")
    return parser.parse_args()


def main() -> int:
    for name in tuple(os.environ):
        if name.upper().startswith("PG"):
            os.environ.pop(name, None)
    result: dict[str, Any] = {
        "status": "failed",
        "reference_sha": REFERENCE_SHA,
        "source_sha256": {
            "rust_test": file_sha256(ROOT / "server" / "tests" / "state_lifecycle_contract.rs"),
            "runner": file_sha256(Path(__file__).resolve()),
            "fixtures": file_sha256(FIXTURE_PATH),
        },
        "cargo_exit_code": None,
        "postgres_stopped": False,
        "temporary_directory_removed": False,
        "covered": [
            "Store permissions, no-op inode/mtime, first empty transaction, atomic failure, and read-only no-write",
            "bidirectional Python 6be and Rust state compatibility",
            "PostgreSQL NUMERIC dscale and Python psycopg digest compatibility",
            "HTTP-owned request survives client disconnect; Tasks close(0/1) aborts and rejects new work",
            "Coordinator drain closes, cancels its owned query, prevents redispatch, and releases state locking",
            "Runtime read-only/check-state no-write; writable migration/readback and fresh-directory startup",
            "legacy OAuth config filtering, profile retirement, and recovery-state cleanup",
        ],
        "not_covered": [
            "production or remote PostgreSQL, external upstream services, and non-loopback HTTP",
            "real user state, deployment data, and platform release binaries",
            "OS power-loss durability beyond the injected rename/persist failure",
        ],
    }
    postgres: http_contract.TemporaryPostgres | None = None
    temporary_path: Path | None = None
    try:
        args = parse_args()
        result["mode"] = "all-rust" if args.all_rust else "state-lifecycle"
        command = command_for(args.all_rust)
        command[0] = args.cargo
        result["cargo_command"] = command
        with tempfile.TemporaryDirectory(prefix="server-state-contract-040-") as temporary:
            temporary_path = Path(temporary)
            reference_root = temporary_path / "reference"
            app_dir = http_contract.archive_reference_app(reference_root, REFERENCE_SHA)
            require(app_dir.is_dir(), "pinned Python reference app was not extracted")
            sys.path.insert(0, str(reference_root))
            from app.policy_store import PolicyStore

            output_root = temporary_path / "state-output"
            output_root.mkdir(mode=0o700)
            cross = output_root / "cross-language"
            cross.mkdir(mode=0o700)
            fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
            require(fixture["reference"]["sha"] == REFERENCE_SHA,
                    "state fixture is not pinned to the expected Python source")
            python_store = PolicyStore(cross / "python-state.json", {})
            with python_store.transaction() as value:
                value.clear()
                value.update(fixture["cross_language"]["python_input"])
            require(python_store.read() == fixture["cross_language"]["python_input"],
                    "pinned Python did not persist the Rust input fixture")

            postgres = http_contract.TemporaryPostgres(temporary_path, args.postgres_bin_dir)
            progress("starting runner-owned loopback PostgreSQL with the application schema")
            postgres.start()
            numeric = verify_numeric_reference(postgres.url, reference_root, fixture)
            result["python_numeric"] = numeric

            cargo_env = safe_environment(temporary_path, postgres.url, output_root)
            progress("running the Rust state lifecycle contract" if not args.all_rust
                     else "running all Rust tests with the same temporary DATABASE_URL")
            completed = subprocess.run(
                command,
                cwd=ROOT,
                env=cargo_env,
                capture_output=True,
                text=True,
                check=False,
            )
            result["cargo_exit_code"] = completed.returncode
            combined = completed.stdout + "\n" + completed.stderr
            result["cargo_test_summaries"] = [
                line.strip() for line in combined.splitlines() if line.lstrip().startswith("test result:")
            ]
            if completed.returncode != 0:
                result["status"] = "failed"
                result["failure_tail"] = sanitized_tail(combined, temporary_path, postgres.url)
            else:
                result["python_readback_checks"] = verify_python_readback(
                    reference_root, output_root, fixture, numeric
                )
                result["status"] = "passed"
            postgres.close()
            result["postgres_stopped"] = not postgres.started
        result["temporary_directory_removed"] = temporary_path is not None and not temporary_path.exists()
        require(result["postgres_stopped"], "runner-owned PostgreSQL did not stop")
        require(result["temporary_directory_removed"], "runner-owned temporary directory was not removed")
        result["cleanup"] = "temporary PostgreSQL stopped and runner-owned files removed"
    except Exception as exc:
        result["status"] = "failed"
        result["error"] = str(exc) if isinstance(exc, (ContractFailure, http_contract.CheckFailure)) else type(exc).__name__
        if postgres is not None and postgres.started:
            try:
                postgres.close()
            except Exception:
                result["postgres_stopped"] = False
        if postgres is not None and not postgres.started:
            result["postgres_stopped"] = True
        if temporary_path is not None:
            result["temporary_directory_removed"] = not temporary_path.exists()
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
