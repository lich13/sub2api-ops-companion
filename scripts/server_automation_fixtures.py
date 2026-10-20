#!/usr/bin/env python3
"""Check pinned Python automation semantics and run the Rust runtime contract.

Run from the repository with the project virtualenv:
  .venv/bin/python scripts/server_automation_fixtures.py
  .venv/bin/python scripts/server_automation_fixtures.py --write-fixtures

The fixture generator extracts app/ from the pinned 0.1.39 commit. The Rust
contract uses runner-owned loopback PostgreSQL and Axum mocks only. A successful
recovery checkpoint is reopened by that pinned Python OAuthStateStore to verify
the persisted receipt envelope remains readable across the migration boundary.
"""
from __future__ import annotations

import argparse
import asyncio
import copy
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
FIXTURE_PATH = ROOT / "tests" / "contracts" / "automation" / "automation-fixtures.json"
TEST_PATH = ROOT / "server" / "tests" / "automation_runtime_contract.rs"
SCHEMA_PATH = ROOT / "tests" / "contracts" / "http" / "schema.sql"
STARTUP_AT = datetime(2031, 4, 5, 6, 7, 8, tzinfo=timezone.utc)
DAILY_TIME = "14:08"
ACCOUNT_ID = 73


class ContractFailure(RuntimeError):
    pass


def require(value: bool, message: str) -> None:
    if not value:
        raise ContractFailure(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def implementation_hash() -> str:
    digest = hashlib.sha256()
    sources = sorted((ROOT / "server" / "src").rglob("*.rs"))
    sources += [ROOT / "server" / "Cargo.toml", ROOT / "server" / "Cargo.lock"]
    for path in sources:
        relative = path.relative_to(ROOT).as_posix()
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def check_reference_commit() -> None:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f"{REFERENCE_SHA}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    require(result.returncode == 0 and result.stdout.strip() == REFERENCE_SHA,
            "pinned automation reference commit is unavailable")


class MemoryScheduleStore:
    def __init__(self, daily: dict[str, Any] | None = None) -> None:
        self.data: dict[str, Any] = {"daily_test": daily or {"batches": {}}}

    def cached_snapshot(self) -> dict[str, Any]:
        return copy.deepcopy(self.data)

    def commit(self, **values: Any) -> None:
        self.data.update(copy.deepcopy(values))


class MemoryAlertStore:
    def snapshot(self) -> dict[str, Any]:
        return {"detection_events": {}, "marks": {}}


class FixtureModelTests:
    def __init__(self, job: dict[str, Any]) -> None:
        self.job = job
        self.get_calls = 0
        self.cancel_calls = 0

    def get(self, _job_id: str) -> dict[str, Any]:
        self.get_calls += 1
        return dict(self.job)

    async def cancel(self, _job_id: str) -> None:
        self.cancel_calls += 1
        self.job["status"] = "cancelled"


class FixtureActions:
    def __init__(self, row: dict[str, Any]) -> None:
        self.row = row

    async def account(self, _account_id: int) -> dict[str, Any]:
        return dict(self.row)


async def detection_scenario(
    model_detection: Any,
    policy_store: Any,
    destination: Path,
    initial_job_status: str,
) -> dict[str, Any]:
    detection = model_detection.ModelDetection.__new__(model_detection.ModelDetection)
    row = {
        "id": ACCOUNT_ID,
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": False,
        "deleted_at": None,
    }
    job = {
        "id": "fixture-model-test-job",
        "account_id": ACCOUNT_ID,
        "status": initial_job_status,
        "detection_generation": 4,
        "detection_mark_version": "fixture-mark-version",
        "completed_at": "2031-04-05T06:07:08+00:00",
        "report": {},
        "triggers": [],
        "error": "synthetic terminal state" if initial_job_status != "running" else "",
    }
    tests = FixtureModelTests(job)
    detection.s = SimpleNamespace(
        actions=FixtureActions(row),
        model_tests=tests,
        invalidate=lambda: None,
    )
    detection.r = SimpleNamespace(
        capacity_alerts=SimpleNamespace(store=MemoryAlertStore()),
    )
    detection.store = policy_store.PolicyStore(
        destination,
        {"accounts": {}, "consumed": {}},
    )
    detection.tick_lock = asyncio.Lock()
    detection.clock = lambda: STARTUP_AT
    control = detection.default()
    control.update(
        enabled=True,
        interval_minutes=1,
        generation=4,
        next_at=(STARTUP_AT + timedelta(minutes=1)).isoformat(),
        status=initial_job_status,
        job_id=job["id"],
    )
    with detection.store.transaction() as state:
        state["accounts"][str(ACCOUNT_ID)] = control

    detection.human_control(ACCOUNT_ID)
    after_manual = detection.control(ACCOUNT_ID)
    statuses = [after_manual["status"]]
    if initial_job_status == "running":
        await detection.tick()
        statuses.append(detection.control(ACCOUNT_ID)["status"])
    await detection.tick()
    final = detection.control(ACCOUNT_ID)
    return {
        "generation_delta": final["generation"] - 4,
        "manual_status": after_manual["status"],
        "interim_statuses": statuses,
        "terminal_status": final["status"],
        "terminal_next_at": final["next_at"],
        "database_schedulable": row["schedulable"],
        "model_get_calls": tests.get_calls,
        "cancel_calls": tests.cancel_calls,
    }


async def detection_reopen_scenario(
    model_detection: Any,
    policy_store: Any,
    destination: Path,
) -> dict[str, Any]:
    detection = model_detection.ModelDetection.__new__(model_detection.ModelDetection)
    row = {
        "id": ACCOUNT_ID,
        "platform": "openai",
        "type": "apikey",
        "status": "active",
        "schedulable": True,
        "deleted_at": None,
    }
    job = {
        "id": "fixture-model-test-job",
        "account_id": ACCOUNT_ID,
        "status": "completed",
        "detection_generation": 4,
        "detection_mark_version": "fixture-mark-version",
        "completed_at": STARTUP_AT.isoformat(),
        "report": {},
        "triggers": [],
        "error": "",
    }
    tests = FixtureModelTests(job)
    detection.s = SimpleNamespace(
        actions=FixtureActions(row),
        model_tests=tests,
        invalidate=lambda: None,
    )
    detection.r = SimpleNamespace(
        capacity_alerts=SimpleNamespace(store=MemoryAlertStore()),
    )
    detection.store = policy_store.PolicyStore(
        destination,
        {"accounts": {}, "consumed": {}, "automatic_starts": {}},
    )
    detection.tick_lock = asyncio.Lock()
    detection.clock = lambda: STARTUP_AT
    control = detection.default()
    control.update(
        enabled=True,
        interval_minutes=1,
        generation=5,
        next_at=None,
        status="paused",
        job_id=job["id"],
    )
    with detection.store.transaction() as state:
        state["accounts"][str(ACCOUNT_ID)] = control
        state["automatic_starts"][str(ACCOUNT_ID)] = {
            "job_id": job["id"],
            "started_at": STARTUP_AT.isoformat(),
        }

    await detection.tick()
    final = detection.control(ACCOUNT_ID)
    return {
        "saved_interval_minutes": final["interval_minutes"],
        "status": final["status"],
        "next_at": final["next_at"],
        "automatic_started_at": STARTUP_AT.isoformat(),
        "next_allowed_at": detection.next_allowed_at(ACCOUNT_ID),
    }


def manual_control_scenario(
    oauth_monitor: Any,
    bark: Any,
    destination: Path,
) -> dict[str, Any]:
    from datetime import datetime, timezone

    store = oauth_monitor.OAuthStateStore(str(destination))
    row = {
        "id": ACCOUNT_ID,
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "rate_limited_at": None,
        "rate_limit_reset_at": None,
        "overload_until": None,
        "temp_unschedulable_until": None,
        "temp_unschedulable_reason": None,
        "extra": {},
        "credentials": {"access_token": "access.example.invalid", "refresh_token": "refresh.example.invalid"},
    }
    daily_key = f"daily:2031-04-05:{ACCOUNT_ID}"
    recovery_key = f"recovery:{ACCOUNT_ID}:fixture:failure:synthetic_error"
    usage_key = f"auth:{ACCOUNT_ID}:active_usage:fixture"

    def seed(state: dict[str, Any]) -> None:
        state["pending_events"][daily_key] = {
            "account_id": ACCOUNT_ID,
            "account_name": "fixture-automation-account",
            "plan_type": "plus",
            "window_labels": ["每日测活"],
            "status": "daily_test_failed",
            "stage": "daily_test",
            "dedupe_key": daily_key,
            "error_code": "fixture_error",
            "error": "synthetic fixture failure",
            "checked_at": "2031-04-05T06:07:08+00:00",
        }
        state["pending_events"][recovery_key] = {
            "account_id": ACCOUNT_ID,
            "account_name": "fixture-automation-account",
            "plan_type": "plus",
            "window_labels": ["5h", "7d"],
            "status": "recovery_failed",
            "stage": "recovery",
            "dedupe_key": recovery_key,
            "error_code": "synthetic_error",
            "error": "synthetic fixture recovery failure",
            "checked_at": "2031-04-05T06:07:08+00:00",
        }
        state["pending_events"][usage_key] = {
            "account_id": ACCOUNT_ID,
            "account_name": "fixture-automation-account",
            "plan_type": "plus",
            "window_labels": ["5h"],
            "status": "auth_failed",
            "stage": "active_usage",
            "dedupe_key": usage_key,
            "error_code": "http_401",
            "error": "synthetic fixture auth error",
            "checked_at": "2031-04-05T06:07:08+00:00",
        }

    store.transaction(seed)
    generation = store.manual_control(row, False, datetime(2031, 4, 5, 6, 7, 8, tzinfo=timezone.utc))
    remaining_before_delivery = store.cached_pending_events()
    pushed: list[tuple[str, str]] = []

    def push(title: str, body: str, _config: Any, *, timeout: float, options: Any = None) -> Any:
        require(timeout == 3, "pinned Bark event delivery changed its timeout")
        pushed.append((title, body))
        return bark.BarkPushResult(True)

    notifier = SimpleNamespace(_push_with_config=push)
    config = bark.BarkRuntimeConfig(
        True,
        True,
        "fixture-bark-device-key",
        "http://127.0.0.1:1",
    )
    for _ in range(2):
        pending = store.cached_pending_events()
        delivered = bark.BarkNotifier.notify_oauth_monitor_events(
            notifier,
            pending,
            config=config,
        )
        store.mark_events_delivered(delivered)
    after = store.snapshot()
    metadata = after["scheduler"].get(str(ACCOUNT_ID), {})
    return {
        "generation": generation,
        "remaining_pending_stages": sorted(event["stage"] for event in remaining_before_delivery),
        "removed_stage": "daily_test",
        "removed_stages": ["daily_test", "recovery"],
        "bark_posts_after_two_deliver_ticks": len(pushed),
        "remaining_pending_count": len(after["pending_events"]),
        "notified_keys": metadata.get("notified_keys", []),
        "active_usage_key": usage_key,
        "removed_daily_key": daily_key,
        "removed_recovery_key": recovery_key,
    }


def receipt_reader_scenario(oauth_monitor: Any, destination: Path) -> dict[str, Any]:
    receipt = {
        "result": {
            "success": True,
            "recovery_method": "connection",
            "recovery_method_generation": 0,
        },
        "credential_fingerprint": "0" * 64,
        "block_state": {"id": ACCOUNT_ID, "status": "active", "schedulable": True},
    }
    destination.write_text(
        json.dumps(
            {
                "version": 5,
                "scheduler": {
                    str(ACCOUNT_ID): {
                        "recovery_method_generation": 0,
                        "recovery_intent": {
                            "status": "testing",
                            "fingerprint": "fixture-reset-boundary",
                            "verification_dispatched": True,
                            "verification_receipt": receipt,
                        },
                    }
                },
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    loaded = oauth_monitor.OAuthStateStore(str(destination)).snapshot()
    intent = loaded["scheduler"][str(ACCOUNT_ID)]["recovery_intent"]
    require(intent["status"] == "retry", "pinned reader did not normalize interrupted testing state")
    require(intent["verification_receipt"] == receipt, "pinned reader did not preserve the receipt envelope")
    return receipt


def build_fixtures(app_root: Path, temporary: Path) -> dict[str, Any]:
    sys.path.insert(0, str(app_root.parent))
    from app.daily_test import DailyTestSchedule, next_daily_time
    from app import bark, model_detection, oauth_monitor, policy_store

    target = next_daily_time(STARTUP_AT, DAILY_TIME)
    stale_state = {
        "batches": {
            "2031-04-03": {"date": "2031-04-03", "status": "running", "accounts": {}},
            "2031-04-04": {"date": "2031-04-04", "status": "queued", "accounts": {}},
        }
    }
    schedule_store = MemoryScheduleStore(stale_state)
    settings = SimpleNamespace(oauth_daily_test_enabled=True, oauth_daily_test_time=DAILY_TIME)
    schedule = DailyTestSchedule(settings, schedule_store, STARTUP_AT)
    startup = schedule_store.cached_snapshot()["daily_test"]
    scheduled = datetime.fromisoformat(startup["next_run_at"])
    on_time_tick = scheduled + timedelta(seconds=30)
    schedule.tick(on_time_tick)
    queued = schedule_store.cached_snapshot()["daily_test"]
    on_time_claim = scheduled + timedelta(seconds=35)
    claimed = schedule.claim(on_time_claim)
    claimed_state = schedule_store.cached_snapshot()["daily_test"]
    late_tick = next_daily_time(on_time_tick, DAILY_TIME) + timedelta(minutes=1, seconds=1)
    schedule.tick(late_tick)
    late_state = schedule_store.cached_snapshot()["daily_test"]

    detection_dir = temporary / "detection"
    detection_dir.mkdir(mode=0o700)
    active = asyncio.run(
        detection_scenario(
            model_detection,
            policy_store,
            detection_dir / "active-model-detection.json",
            "running",
        )
    )
    terminal = asyncio.run(
        detection_scenario(
            model_detection,
            policy_store,
            detection_dir / "terminal-model-detection.json",
            "completed",
        )
    )
    reopened = asyncio.run(
        detection_reopen_scenario(
            model_detection,
            policy_store,
            detection_dir / "manual-reopen-model-detection.json",
        )
    )
    manual = manual_control_scenario(oauth_monitor, bark, temporary / "oauth-state.json")
    receipt = receipt_reader_scenario(oauth_monitor, temporary / "receipt-reader.json")

    model_detection_source = (app_root / "model_detection.py").read_text(encoding="utf-8")
    oauth_source = (app_root / "oauth_monitor.py").read_text(encoding="utf-8")
    require("generation=value['generation']+1" in model_detection_source,
            "pinned ModelDetection.human_control no longer invalidates worker generations")
    require("if value['generation'] == job.get('detection_generation')" in model_detection_source,
            "pinned detection completion no longer guards timer writes by generation")
    require("if intent.get('verification_dispatched') and (not intent.get('verification_receipt')" in oauth_source,
            "pinned monitor no longer blocks unknown dispatched verification from replay")
    require("intent.update(status='blocked', last_error_code='verification_uncertain'" in oauth_source,
            "pinned monitor no longer classifies an unknown verification result")

    return {
        "reference": {"sha": REFERENCE_SHA, "daily_time_beijing": DAILY_TIME},
        "daily_test": {
            "startup_at": STARTUP_AT.isoformat(),
            "scheduled_at": target.isoformat(),
            "on_time_tick_at": on_time_tick.isoformat(),
            "on_time_claim_at": on_time_claim.isoformat(),
            "next_run_at": claimed_state["next_run_at"],
            "late_tick_at": late_tick.isoformat(),
            "late_batch_date": late_state["batches"]["2031-04-06"]["date"],
            "startup_stale_status": startup["batches"]["2031-04-03"]["status"],
            "on_time_status": queued["batches"]["2031-04-05"]["status"],
            "claimed_status": claimed["status"],
            "late_status": late_state["batches"]["2031-04-06"]["status"],
        },
        "detection": {
            "manual_close": {
                "generation_delta": active["generation_delta"],
                "database_schedulable": active["database_schedulable"],
                "active_interim_statuses": active["interim_statuses"],
                "terminal_status": terminal["terminal_status"],
                "terminal_next_at": terminal["terminal_next_at"],
                "max_model_dispatches": 1,
            },
            "legacy_source_rules": {
                "human_control_invalidates_generation": True,
                "active_branch_checks_worker_guard": True,
                "terminal_timer_requires_matching_generation": True,
            },
            "legacy_observation": {
                "active_cancel_calls": active["cancel_calls"],
                "terminal_manual_status": terminal["manual_status"],
                "terminal_generation_delta": terminal["generation_delta"],
                "terminal_next_at": terminal["terminal_next_at"],
            },
            "manual_reopen": reopened,
        },
        "manual_control": {
            "generation": manual["generation"],
            "remaining_pending_stages": manual["remaining_pending_stages"],
            "removed_stage": manual["removed_stage"],
            "removed_stages": manual["removed_stages"],
            "bark_posts_after_two_deliver_ticks": manual["bark_posts_after_two_deliver_ticks"],
            "remaining_pending_count": manual["remaining_pending_count"],
            "notified_keys": [
                manual["active_usage_key"]
            ] if manual["active_usage_key"] in manual["notified_keys"] else [],
            "removed_daily_key": manual["removed_daily_key"],
        },
        "recovery_receipt": {
            "format": "legacy-envelope",
            "required_keys": sorted(receipt),
            "required_result_fields": ["success", "recovery_method", "recovery_method_generation"],
            "fingerprint_length": len(receipt["credential_fingerprint"]),
            "unknown_verification_error": "verification_uncertain",
            "unknown_max_test_dispatches": 1,
            "recovery_write_dispatches_after_crash": 1,
        },
    }


def safe_environment(database_url: str, temporary: Path, export_dir: Path) -> dict[str, str]:
    env = filtered_mapping(os.environ.copy())
    scratch = temporary / "scratch"
    scratch.mkdir(mode=0o700, exist_ok=True)
    env.update({
        "DATABASE_URL": database_url,
        "KEEPALIVE_CONFIG_PATH": str(temporary / "keepalive-state.json"),
        "TMPDIR": str(scratch),
        "TMP": str(scratch),
        "TEMP": str(scratch),
        "NO_PROXY": "127.0.0.1,localhost,::1",
        "no_proxy": "127.0.0.1,localhost,::1",
        "CARGO_NET_OFFLINE": "true",
        "RUST_TEST_THREADS": "1",
        "AUTOMATION_STATE_EXPORT_DIR": str(export_dir),
    })
    return env


def filtered_mapping(source: dict[str, str]) -> dict[str, str]:
    env = source.copy()
    secret_markers = (
        "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "API_KEY", "APIKEY",
        "PRIVATE_KEY", "PRIVATEKEY", "COOKIE", "ACCESS_KEY", "ACCESSKEY",
    )
    sensitive_prefixes = (
        "AWS_", "AZURE_", "GITHUB_", "OPENAI_", "ANTHROPIC_", "BARK_",
        "OAUTH_", "SUB2API_", "SUB2OPS_", "SSH_", "PGPASSWORD",
    )
    for name in tuple(env):
        upper = name.upper()
        if any(marker in upper for marker in secret_markers) or upper.startswith(sensitive_prefixes):
            env.pop(name, None)
    for name in (
        "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
    ):
        env.pop(name, None)
    return env


@contextmanager
def filtered_environment():
    original = os.environ.copy()
    filtered = filtered_mapping(original)
    try:
        os.environ.clear()
        os.environ.update(filtered)
        yield
    finally:
        os.environ.clear()
        os.environ.update(original)


def scrub(text: str, temporary: Path, database_url: str) -> str:
    text = text.replace(str(ROOT), "<repository>")
    text = text.replace(str(temporary), "<runner-temp>")
    text = text.replace(database_url, "<temporary-database-url>")
    text = re.sub(
        r"(?i)(authorization|x-api-key|database_url|password)\s*[:=]\s*[^\s,]+",
        r"\1=<redacted>",
        text,
    )
    return text


def cargo_summary(output: str) -> dict[str, Any]:
    lines = re.findall(r"^\s*test ([A-Za-z0-9_:]+) \.\.\. (ok|FAILED)$", output, re.M)
    summaries = re.findall(
        r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; "
        r"(\d+) ignored; (\d+) measured; (\d+) filtered out",
        output,
    )
    result: dict[str, Any] = {
        "passed_tests": [name for name, status in lines if status == "ok"],
        "failed_tests": [name for name, status in lines if status == "FAILED"],
    }
    if summaries:
        status, passed, failed, ignored, measured, filtered = summaries[-1]
        result["test_result"] = {
            "status": "passed" if status == "ok" else "failed",
            "passed": int(passed),
            "failed": int(failed),
            "ignored": int(ignored),
            "measured": int(measured),
            "filtered_out": int(filtered),
        }
    else:
        result["test_result"] = None
    return result


def cargo_diagnostics(output: str) -> list[str]:
    diagnostics = []
    for line in output.splitlines():
        clean = re.sub(r"\x1b\[[0-9;]*m", "", line)
        if re.match(r"^\s*error(?:\[E\d+\])?: ", clean):
            diagnostics.append(clean.strip())
        elif re.match(r"^\s*test [A-Za-z0-9_:]+ \.\.\. FAILED$", clean):
            diagnostics.append(clean.strip())
        elif "panicked at" in clean:
            location = re.search(r"([^ ]+\.rs:\d+:\d+)", clean)
            diagnostics.append("panic at " + location.group(1) if location else "test panic")
        elif re.match(r"^\s*assertion .{1,180} failed$", clean):
            diagnostics.append(clean.strip())
        elif re.match(r"^\s*(?:left|right): ", clean):
            diagnostics.append(clean.strip()[:240])
    return diagnostics[:30]


def process_absent(pid: int | None) -> bool:
    if pid is None:
        return True
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    return False


def terminate_process_group(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)


def check_exported_receipt(app_root: Path, export_path: Path) -> dict[str, Any]:
    require(export_path.is_file(), "Rust recovery checkpoint export is missing")
    sys.path.insert(0, str(app_root.parent))
    from app.oauth_monitor import OAuthStateStore

    state = OAuthStateStore(str(export_path)).snapshot()
    candidates = [
        metadata.get("recovery_intent") or {}
        for metadata in state.get("scheduler", {}).values()
    ]
    intent = next(
        (item for item in candidates if item.get("verification_receipt")),
        None,
    )
    require(intent is not None, "pinned reader did not recover the Rust verification receipt")
    receipt = intent["verification_receipt"]
    required_keys = {"result", "credential_fingerprint", "block_state"}
    allowed_keys = required_keys | {"manual_generation"}
    require(set(receipt) == allowed_keys,
            "Rust receipt contains fields outside the pinned envelope and manual-generation extension")
    require(isinstance(receipt["result"], dict), "receipt result is not an object")
    require(len(receipt["credential_fingerprint"]) == 64,
            "receipt credential fingerprint is not a SHA-256 hex string")
    require(isinstance(receipt["block_state"], dict), "receipt block state is not an object")
    require(receipt["result"].get("success") is True, "pinned reader did not preserve successful verification")
    require(receipt["result"].get("recovery_method") == "connection",
            "pinned reader did not preserve the selected recovery method")
    scheduler_meta = state.get("scheduler", {}).get(str(ACCOUNT_ID), {})
    control = scheduler_meta.get("manual_control") or {}
    expected_generation = control.get("generation", 0)
    require(type(expected_generation) is int, "pinned reader did not preserve manual generation state")
    require(receipt["manual_generation"] == expected_generation,
            "pinned reader did not preserve receipt manual_generation")
    return {
        "status": "readable",
        "receipt_keys": sorted(receipt),
        "result_success": receipt["result"]["success"],
        "recovery_method": receipt["result"]["recovery_method"],
        "fingerprint_length": len(receipt["credential_fingerprint"]),
        "manual_generation": receipt["manual_generation"],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--postgres-bin-dir",
        help="directory containing initdb and pg_ctl when they are not on PATH",
    )
    parser.add_argument(
        "--write-fixtures",
        action="store_true",
        help="regenerate the synthetic compatibility fixture from pinned Python 6be",
    )
    parser.add_argument(
        "--fixtures-only",
        action="store_true",
        help="check the pinned Python fixture without starting PostgreSQL or Cargo",
    )
    return parser.parse_args()


def run(args: argparse.Namespace) -> dict[str, Any]:
    result: dict[str, Any] = {
        "status": "failed",
        "reference_sha": REFERENCE_SHA,
        "rust_target": "automation_runtime_contract",
        "postgres": {"kind": "temporary PostgreSQL", "loopback_only": True, "schema": "tests/contracts/http/schema.sql"},
        "upstream_mock": "Rust Axum loopback fixture",
        "source_sha256": {
            "automation_runtime_contract.rs": sha256(TEST_PATH) if TEST_PATH.is_file() else None,
            "automation-fixtures.json": sha256(FIXTURE_PATH) if FIXTURE_PATH.is_file() else None,
            "runner": sha256(Path(__file__).resolve()),
            "schema.sql": sha256(SCHEMA_PATH) if SCHEMA_PATH.is_file() else None,
            "implementation": implementation_hash(),
        },
        "implementation_sha256_after": None,
        "exit_code": None,
    }
    require(TEST_PATH.is_file() and FIXTURE_PATH.is_file(), "Rust automation source or fixture is missing")
    require(SCHEMA_PATH.is_file(), "temporary application schema fixture is missing")
    check_reference_commit()

    with tempfile.TemporaryDirectory(prefix="server-automation-contract-") as name:
        temporary = Path(name)
        with filtered_environment():
            scripts = ROOT / "scripts"
            if str(scripts) not in sys.path:
                sys.path.insert(0, str(scripts))
            from server_http_contract import TemporaryPostgres, archive_reference_app

            reference_dir = temporary / "reference"
            app_root = archive_reference_app(reference_dir, REFERENCE_SHA)
            fixture = build_fixtures(app_root, temporary)

            if args.write_fixtures:
                FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
                FIXTURE_PATH.write_text(
                    json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                result["fixture_action"] = "written from pinned Python reference"
                result["source_sha256"]["automation-fixtures.json"] = sha256(FIXTURE_PATH)
                result["status"] = "fixtures_written"
                result["exit_code"] = 0
                return result

            expected = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
            require(fixture == expected, "pinned Python semantic output differs from automation fixture")
            result["fixture_action"] = "matched pinned Python runtime semantics"
            result["fixture_summary"] = {
                "daily_on_time": fixture["daily_test"]["on_time_status"],
                "daily_late": fixture["daily_test"]["late_status"],
                "manual_active_interim": fixture["detection"]["manual_close"]["active_interim_statuses"],
                "manual_terminal": fixture["detection"]["manual_close"]["terminal_status"],
                "manual_retained_stages": fixture["manual_control"]["remaining_pending_stages"],
                "bark_posts_after_two_ticks": fixture["manual_control"]["bark_posts_after_two_deliver_ticks"],
                "receipt_envelope": fixture["recovery_receipt"]["required_keys"],
            }
            if args.fixtures_only:
                result["status"] = "fixtures_passed"
                result["exit_code"] = 0
                return result

            postgres = TemporaryPostgres(temporary, args.postgres_bin_dir)
            try:
                with filtered_environment():
                    postgres.start()
                result["postgres"]["started"] = True
                result["postgres"]["pid"] = postgres.postgres_pid
                result["source_sha256"]["implementation"] = implementation_hash()
                export_dir = temporary / "exports"
                export_dir.mkdir(mode=0o700)
                env = safe_environment(postgres.url, temporary, export_dir)
                command = [
                    "cargo", "test", "--manifest-path", "server/Cargo.toml",
                    "--locked", "--offline", "--features", "test-fixtures",
                    "--test", "automation_runtime_contract",
                    "--", "--test-threads=1", "--nocapture",
                ]
                process = subprocess.Popen(
                    command,
                    cwd=ROOT,
                    env=env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    start_new_session=True,
                )
                try:
                    stdout, stderr = process.communicate(timeout=300)
                except subprocess.TimeoutExpired:
                    terminate_process_group(process)
                    stdout, stderr = process.communicate()
                    output = (stdout or "") + "\n" + (stderr or "")
                    result["exit_code"] = 124
                    result["cargo_timeout_seconds"] = 300
                else:
                    output = (stdout or "") + "\n" + (stderr or "")
                    result["exit_code"] = process.returncode
                result["tests"] = cargo_summary(output)
                result["diagnostics"] = [
                    scrub(line, temporary, postgres.url)
                    for line in cargo_diagnostics(output)
                ]
                result["implementation_sha256_after"] = implementation_hash()
                require(
                    result["implementation_sha256_after"] == result["source_sha256"]["implementation"],
                    "Rust implementation changed while automation Cargo target was running",
                )
                exported_state = export_dir / "automation-recovery-receipt.json"
                if exported_state.is_file():
                    try:
                        result["python_receipt_reader"] = check_exported_receipt(app_root, exported_state)
                    except BaseException as exc:
                        result["python_receipt_reader"] = {"status": "failed", "error": type(exc).__name__}
                else:
                    result["python_receipt_reader"] = {"status": "missing"}
            finally:
                pg_pid = postgres.postgres_pid
                try:
                    with filtered_environment():
                        postgres.close()
                    result["postgres"]["stopped"] = not postgres.started
                    result["postgres"]["process_absent"] = process_absent(pg_pid)
                except BaseException as exc:
                    result["cleanup_error"] = type(exc).__name__
                    result["postgres"]["stopped"] = False

    result["temporary_directory_removed"] = not temporary.exists()
    summary = result.get("tests", {}).get("test_result") or {}
    passed = (
        result.get("exit_code") == 0
        and summary.get("status") == "passed"
        and summary.get("passed", 0) == 11
        and summary.get("failed", 0) == 0
        and result.get("python_receipt_reader", {}).get("status") == "readable"
        and result.get("postgres", {}).get("stopped") is True
        and result.get("postgres", {}).get("process_absent") is True
        and result.get("temporary_directory_removed") is True
        and not result.get("cleanup_error")
    )
    result["status"] = "passed" if passed else "failed"
    if not passed and result.get("exit_code") == 0:
        result["exit_code"] = 1
    return result


def main() -> int:
    try:
        result = run(parse_args())
    except KeyboardInterrupt:
        result = {"status": "cancelled", "error": "interrupted", "exit_code": 130}
    except BaseException as exc:
        result = {"status": "failed", "error": type(exc).__name__, "exit_code": 1}
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return int(result.get("exit_code") if result.get("exit_code") is not None else 1)


if __name__ == "__main__":
    raise SystemExit(main())
