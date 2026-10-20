#!/usr/bin/env python3
"""Compare fixed Python and Rust runtimes under synthetic history load.

This Linux-only regression reuses the service, PostgreSQL, mock and cgroup
helpers from server_benchmark.py. It makes only loopback GET requests during
measurement; automatic recovery, daily testing, credit reset and Bark are off.

Examples:
  python scripts/server_history_memory.py --python-image sub2ops-companion:python-reference --rust-image sub2ops-companion:ci
  python scripts/server_history_memory.py --implementation rust --rust-image sub2ops-companion:ci
"""
from __future__ import annotations

import argparse
from collections.abc import Collection
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path
import platform
import tempfile
import time

import psycopg

from server_benchmark import Native, Sampler, distribution, service as benchmark_service
from server_http_contract import (
    ADMIN_KEY,
    API,
    REFERENCE_SHA,
    CheckFailure,
    TemporaryPostgres,
    archive_reference_app,
    require,
    seed_fingerprint_cache,
)


ACCOUNT_COUNT = 20
USAGE_COUNT = 100_000
ERROR_COUNT = 3_000
OPERATIONS_COUNT = 4_000
OPERATIONS_TARGET_BYTES = 1_700_000
DETECTION_COUNT = 4_000
MANUAL_TEST_COUNT = 400
IDLE_SECONDS = 30.0
READ_ROUND_COUNT = 3
MIN_READ_ROUND_INTERVAL_SECONDS = 31.0
DEFAULT_RUST_MEMORY_LIMIT_BYTES = 512 * 1024 * 1024
GROWTH_ABSOLUTE_TOLERANCE_BYTES = 64 * 1024 * 1024
GROWTH_RELATIVE_TOLERANCE = 0.10
HISTORY_FIXTURE_SPAN_SECONDS = 600_000


def source_sha256() -> str:
    return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def history_json_bytes(value: dict) -> bytes:
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode("ascii")


def write_operation_history(state_dir: Path) -> dict[str, int]:
    """Write a bounded, completed-only state file shared by both runtimes."""
    at = datetime.now(timezone.utc)
    note_prefix = "synthetic completed history fixture:"
    jobs: dict[str, dict] = {}
    last_id = ""
    for number in range(1, OPERATIONS_COUNT + 1):
        job_id = f"{number:032x}"
        last_id = job_id
        stamp = (at - timedelta(minutes=number)).isoformat(timespec="microseconds").replace("+00:00", "Z")
        priority = number % 10
        jobs[job_id] = {
            "id": job_id,
            "account_id": (number - 1) % ACCOUNT_COUNT + 1,
            "action": "priority",
            "payload": {"priority": priority},
            "created_at": stamp,
            "updated_at": stamp,
            "status": "completed",
            "reason": "",
            "result": {"verified": True, "history_note": note_prefix},
        }

    document = {"version": 1, "jobs": jobs, "requests": {}, "batches": {}, "batch_requests": {}}
    base = history_json_bytes(document)
    remaining = OPERATIONS_TARGET_BYTES - len(base)
    require(remaining > 0, "synthetic operation history base exceeds its target size")
    per_job, remainder = divmod(remaining, OPERATIONS_COUNT)
    for job in jobs.values():
        job["result"]["history_note"] += "x" * per_job
    jobs[last_id]["result"]["history_note"] += "x" * remainder
    encoded = history_json_bytes(document)
    require(len(encoded) == OPERATIONS_TARGET_BYTES, "synthetic operation history size differs from its target")

    path = state_dir / "account-operations.json"
    path.write_bytes(encoded)
    path.chmod(0o600)
    return {"jobs": len(jobs), "bytes": len(encoded)}


def write_detection_history(state_dir: Path) -> dict[str, object]:
    """Completed synthetic jobs include summaries after detailed reports expire."""
    at = datetime.now(timezone.utc)
    jobs = {}
    needs_confirmation_ids: list[str] = []
    statuses = ("completed", "completed", "failed", "cancelled", "interrupted", "needs_confirmation")
    for number in range(1, DETECTION_COUNT + MANUAL_TEST_COUNT + 1):
        job_id = f"{number:032x}"
        stamp = (at - timedelta(seconds=number // 2)).isoformat(timespec="microseconds").replace("+00:00", "Z")
        status = statuses[number % len(statuses)]
        analyzed = status == "completed"
        degraded = analyzed and number % 2 == 0
        jobs[job_id] = {
            "id": job_id, "account_id": (number - 1) % ACCOUNT_COUNT + 1,
            "account_name": "fixture-detection-account", "requested_model": "gpt-6-luna",
            "automatic": number <= DETECTION_COUNT, "status": status,
            "queued_at": stamp, "started_at": stamp, "completed_at": stamp,
            "triggers": ["scheduled"], "groups": [], "report": None,
            "result_summary": {
                "outcome": "degraded" if degraded else "normal" if analyzed else status,
                "analysis_available": analyzed,
                "prediction": "gpt-5.6-luna" if degraded else "gpt-6-luna" if analyzed else None,
                "first_prediction": "gpt-5.6-luna" if degraded else "gpt-6-luna" if analyzed else None,
                "probability": 0.95 if analyzed else None, "used_outputs": 3 if analyzed else None,
                "first_downgrade": degraded, "aggregate_downgrade": degraded,
            },
        }
        if status == "needs_confirmation":
            needs_confirmation_ids.append(job_id)
    encoded = history_json_bytes({"version": 1, "accounts": {}, "requests": {}, "jobs": jobs})
    path = state_dir / "model-test-results.json"
    path.write_bytes(encoded)
    path.chmod(0o600)
    return {
        "automatic_jobs": DETECTION_COUNT,
        "manual_jobs": MANUAL_TEST_COUNT,
        "needs_confirmation_model_ids": needs_confirmation_ids,
        "bytes": len(encoded),
    }


def expected_completed_operation_ids() -> frozenset[str]:
    """Return the newest completed operation IDs retained by the listing."""
    return frozenset(f"{number:032x}" for number in range(1, 31))


def validate_operation_history(history: dict, expected_model_ids: Collection[str]) -> dict[str, int]:
    """Validate the mixed operation listing without treating all items as completed."""
    items = history.get("items")
    pending = history.get("pending")
    require(isinstance(items, list), "synthetic operation history items are not a list")
    require(isinstance(pending, int), "synthetic operation history pending count is invalid")

    expected_completed_ids = expected_completed_operation_ids()
    expected_model_ids = frozenset(expected_model_ids)
    completed_items = [
        item for item in items
        if isinstance(item, dict) and item.get("action") == "priority"
    ]
    model_items = [
        item for item in items
        if isinstance(item, dict) and item.get("action") == "model_test"
    ]

    require(len(completed_items) == len(expected_completed_ids),
            "runtime operation history did not return exactly 30 priority items")
    require(all(item.get("status") == "completed" for item in completed_items),
            "runtime operation history returned a non-completed priority item")
    completed_ids = [item.get("id") for item in completed_items]
    require(all(isinstance(job_id, str) for job_id in completed_ids),
            "runtime operation history returned a priority item without an ID")
    require(len(completed_ids) == len(set(completed_ids)),
            "runtime operation history returned duplicate priority items")
    require(set(completed_ids) == expected_completed_ids,
            "runtime operation history returned unexpected priority items")

    require(len(model_items) == len(expected_model_ids),
            "runtime operation history returned an unexpected model-test item count")
    require(all(item.get("status") == "needs_confirmation" for item in model_items),
            "runtime operation history returned a model-test item in an unexpected state")
    model_ids = [item.get("id") for item in model_items]
    require(all(isinstance(job_id, str) for job_id in model_ids),
            "runtime operation history returned a model-test item without an ID")
    require(len(model_ids) == len(set(model_ids)),
            "runtime operation history returned duplicate model-test items")
    require(set(model_ids) == expected_model_ids,
            "runtime operation history omitted or added model-test items")

    require(len(items) == len(completed_items) + len(model_items),
            "runtime operation history returned an unexpected operation kind")
    require(pending == len(expected_model_ids),
            "synthetic operation history pending count differs from expected model tests")
    return {
        "returned_completed_items": len(completed_items),
        "returned_items": len(items),
        "expected_pending_model_test_items": len(expected_model_ids),
    }


def read_detection_history(client, state_dir: Path) -> tuple[int, list[float]]:
    path = state_dir / "model-test-results.json"
    before = hashlib.sha256(path.read_bytes()).digest()
    cursor = None
    seen = set()
    previous_key = None
    latencies = []
    while True:
        suffix = "" if cursor is None else "&before=" + cursor
        page, latency = api_get(client, "/detection-logs?limit=100" + suffix)
        latencies.append(latency)
        items = page.get("items")
        require(isinstance(items, list) and 0 < len(items) <= 100, "detection log page has invalid size")
        for item in items:
            job_id = item.get("job_id")
            require(isinstance(job_id, str) and job_id not in seen, "detection log contains duplicate jobs")
            require(1 <= int(job_id, 16) <= DETECTION_COUNT, "detection log contains manual or unknown jobs")
            key = (item.get("created_at"), job_id)
            require(previous_key is None or key < previous_key, "detection log pagination is not strictly ordered")
            previous_key = key
            seen.add(job_id)
            require(isinstance(item.get("summary"), dict) and item.get("details_available") is False,
                    "detection log lost its summary after report cleanup")
        next_cursor = page.get("next_cursor")
        if next_cursor is None:
            break
        require(isinstance(next_cursor, str) and next_cursor and next_cursor != cursor,
                "detection log continuation cursor did not advance")
        require(len(seen) < DETECTION_COUNT, "detection log cursor extends past the fixture")
        cursor = next_cursor
    require(len(seen) == DETECTION_COUNT, "detection log pagination omitted automatic jobs")
    require(hashlib.sha256(path.read_bytes()).digest() == before, "reading detection logs changed persisted jobs")
    return len(seen), latencies


def seed_synthetic_dataset(database_url: str) -> dict[str, int]:
    """Replace schema examples with the complete synthetic seven-day workload.

    Keep the oldest fixture rows four hours inside the seven-day boundary so
    seed and test startup time cannot age them out of the assertion window.
    """
    with psycopg.connect(database_url, autocommit=True) as connection:
        connection.execute("TRUNCATE accounts CASCADE")
        connection.execute(
            """
            INSERT INTO accounts (
                id, name, platform, type, status, schedulable, credentials,
                extra, priority, concurrency, load_factor
            )
            SELECT id, 'fixture-history-account-' || id::text, 'openai', 'oauth',
                   'active', false, '{}'::jsonb, '{}'::jsonb, 3, 2, 1
            FROM generate_series(1, 20) AS fixture(id)
            """
        )
        connection.execute(
            """
            INSERT INTO usage_logs (
                id, account_id, created_at, model, upstream_model, stream,
                input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                duration_ms, first_token_ms, reasoning_effort, service_tier,
                openai_ws_mode, inbound_endpoint
            )
            SELECT n,
                   ((n - 1) % 20) + 1,
                   now() - (((n - 1) % {HISTORY_FIXTURE_SPAN_SECONDS}) * interval '1 second'),
                   'fixture-model-' || ((((n - 1) / 20) % 5) + 1)::text,
                   'fixture-model-' || ((((n - 1) / 20) % 5) + 1)::text,
                   true,
                   128 + (n % 2048),
                   32 + (n % 256),
                   n % 13,
                   n % 101,
                   2000 + (n % 10000),
                   100 + (n % 700),
                   'default', 'default', false, '/v1/responses'
            FROM generate_series(1, 100000) AS fixture(n)
            """.format(HISTORY_FIXTURE_SPAN_SECONDS=HISTORY_FIXTURE_SPAN_SECONDS)
        )
        connection.execute(
            """
            INSERT INTO ops_error_logs (
                id, account_id, created_at, platform, error_owner, error_phase,
                error_source, stream, model, requested_model, upstream_model,
                status_code, upstream_status_code, provider_error_code,
                provider_error_type, error_type, error_message,
                upstream_error_message, request_id, client_request_id, resolved
            )
            SELECT 2000000 + n,
                   ((n - 1) % 20) + 1,
                   now() - (((n - 1) % {HISTORY_FIXTURE_SPAN_SECONDS}) * interval '1 second'),
                   'openai', 'provider', 'upstream', 'synthetic', true,
                   'fixture-model-' || ((((n - 1) / 20) % 5) + 1)::text,
                   'fixture-model-' || ((((n - 1) / 20) % 5) + 1)::text,
                   'fixture-model-' || ((((n - 1) / 20) % 5) + 1)::text,
                   CASE WHEN n % 3 = 0 THEN 500 ELSE 429 END,
                   CASE WHEN n % 3 = 0 THEN 500 ELSE 429 END,
                   CASE WHEN n % 3 = 0 THEN 'fixture_upstream_error' ELSE 'fixture_rate_limit' END,
                   'synthetic_provider_error', 'synthetic_upstream_error',
                   'synthetic history fixture error', 'synthetic upstream fixture error',
                   'fixture-history-error-' || n::text,
                   'fixture-history-client-error-' || n::text,
                   false
            FROM generate_series(1, 3000) AS fixture(n)
            """.format(HISTORY_FIXTURE_SPAN_SECONDS=HISTORY_FIXTURE_SPAN_SECONDS)
        )
    return assert_synthetic_dataset(database_url)


def assert_synthetic_dataset(database_url: str) -> dict[str, int]:
    with psycopg.connect(database_url) as connection:
        row = connection.execute(
            """
            SELECT
                (SELECT count(*) FROM accounts),
                (SELECT count(*) FROM accounts WHERE created_at >= now() - interval '7 days' AND created_at <= now()),
                (SELECT count(*) FROM usage_logs),
                (SELECT count(*) FROM usage_logs WHERE created_at >= now() - interval '7 days' AND created_at <= now()),
                (SELECT count(*) FROM ops_error_logs),
                (SELECT count(*) FROM ops_error_logs WHERE created_at >= now() - interval '7 days' AND created_at <= now())
            """
        ).fetchone()
    expected = (20, 20, USAGE_COUNT, USAGE_COUNT, ERROR_COUNT, ERROR_COUNT)
    require(tuple(row) == expected, "synthetic database totals or seven-day counts differ")
    return {
        "accounts_total": row[0],
        "accounts_7d": row[1],
        "usage_total": row[2],
        "usage_7d": row[3],
        "errors_total": row[4],
        "errors_7d": row[5],
    }


def api_get(client, path: str) -> tuple[dict, float]:
    started = time.monotonic()
    response = client.request("GET", API + path, headers={"x-api-key": ADMIN_KEY})
    elapsed_ms = (time.monotonic() - started) * 1000
    require(response.status == 200, "synthetic history API GET returned an unexpected status")
    require(isinstance(response.body, dict), "synthetic history API did not return an object")
    return response.body, elapsed_ms


def sampler_rows(sampler: Sampler, start: float, finish: float) -> list[tuple[float, int, int]]:
    with sampler.lock:
        return sorted(row for row in sampler.samples if start <= row[0] <= finish)


def memory_p95(rows: list[tuple[float, int, int]]) -> int:
    result = distribution([float(row[1]) for row in rows])
    require(result["p95"] is not None, "insufficient cgroup memory samples")
    return int(result["p95"])


def resource_window(sampler: Sampler, start: float, finish: float) -> dict:
    metrics = sampler.metrics(start, finish)
    metrics["memory_p95_bytes"] = memory_p95(sampler_rows(sampler, start, finish))
    return metrics


def wait_until(deadline: float) -> None:
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        time.sleep(min(0.2, remaining))


def measure_runtime(
    *,
    implementation: str,
    image: str,
    database_url: str,
    reference_app: Path,
    root: Path,
    memory_limit_bytes: int,
) -> dict:
    dataset = assert_synthetic_dataset(database_url)
    state_dir = root / f"state-{implementation}"
    state_dir.mkdir(mode=0o700)
    operation_history = write_operation_history(state_dir)
    detection_fixture = write_detection_history(state_dir) if implementation == "rust" else None
    expected_model_ids = frozenset(
        detection_fixture["needs_confirmation_model_ids"]
        if detection_fixture is not None else ()
    )
    detection_history = (
        {
            key: value for key, value in detection_fixture.items()
            if key != "needs_confirmation_model_ids"
        }
        if detection_fixture is not None else None
    )
    seed_fingerprint_cache(reference_app, state_dir)

    with Native(0.0) as native:
        with benchmark_service(image, implementation, database_url, native.url, state_dir) as (
            container,
            client,
            shutdown,
        ):
            sampler = Sampler(container)
            try:
                sampler_start = sampler.mark()
                time.sleep(IDLE_SECONDS)
                idle_finish = sampler.mark()
                idle = resource_window(sampler, sampler_start, idle_finish)

                phase_start = sampler.mark()
                snapshot_latencies: list[float] = []
                operation_latencies: list[float] = []
                usage_latencies: list[float] = []
                error_latencies: list[float] = []
                detection_latencies: list[float] = []
                rounds: list[dict] = []
                expected_ids = list(range(1, ACCOUNT_COUNT + 1))
                for index in range(1, READ_ROUND_COUNT + 1):
                    trigger_started = sampler.mark()

                    history, history_latency = api_get(client, "/account-operations")
                    operation_latencies.append(history_latency)
                    operation_result = validate_operation_history(history, expected_model_ids)
                    items = history["items"]

                    snapshot, snapshot_latency = api_get(client, "/snapshot")
                    snapshot_latencies.append(snapshot_latency)
                    accounts = snapshot.get("accounts")
                    require(isinstance(accounts, list) and len(accounts) == ACCOUNT_COUNT,
                            "snapshot account count differs from the synthetic dataset")
                    actual_ids = sorted(account.get("id") for account in accounts)
                    require(actual_ids == expected_ids, "snapshot account IDs differ from the synthetic dataset")

                    usage, usage_latency = api_get(client, "/usage-records?account_id=1&limit=100")
                    usage_latencies.append(usage_latency)
                    usage_items = usage.get("items")
                    require(isinstance(usage_items, list) and len(usage_items) == 100,
                            "usage query did not return the expected synthetic page")
                    require(all(item.get("account_id") == 1 for item in usage_items),
                            "usage query returned a row outside the requested account")
                    require(usage.get("next_cursor") is not None,
                            "usage query did not expose a continuation cursor for the synthetic history")

                    errors, error_latency = api_get(client, "/errors?account_id=1&limit=100")
                    error_latencies.append(error_latency)
                    error_items = errors.get("items")
                    require(isinstance(error_items, list) and len(error_items) == 100,
                            "error query did not return the expected synthetic page")
                    require(all(item.get("account_id") == 1 for item in error_items),
                            "error query returned a row outside the requested account")
                    require(errors.get("next_cursor") is not None,
                            "error query did not expose a continuation cursor for the synthetic history")

                    detection_items = None
                    if implementation == "rust":
                        detection_items, page_latencies = read_detection_history(client, state_dir)
                        detection_latencies.extend(page_latencies)

                    completion_time = time.monotonic()
                    due = trigger_started + MIN_READ_ROUND_INTERVAL_SECONDS
                    wait_until(due)
                    round_finish = sampler.mark()
                    rounds.append({
                        "round": index,
                        "snapshot_latency_ms": round(snapshot_latency, 3),
                        "operation_history_latency_ms": round(history_latency, 3),
                        "operation_history_items": len(items),
                        "usage_latency_ms": round(usage_latency, 3),
                        "usage_items": len(usage_items),
                        "usage_next_cursor_present": True,
                        "error_latency_ms": round(error_latency, 3),
                        "error_items": len(error_items),
                        "error_next_cursor_present": True,
                        "detection_log_items": detection_items,
                        "completion_seconds": round(completion_time - trigger_started, 3),
                        "resource_window": resource_window(sampler, trigger_started, round_finish),
                        "trigger_interval_seconds": round(round_finish - trigger_started, 3),
                    })

                finish = sampler.mark()
                overall = resource_window(sampler, sampler_start, finish)
                trend_rows = sampler_rows(sampler, phase_start, finish)
                midpoint = phase_start + (finish - phase_start) / 2
                first_half = [row for row in trend_rows if row[0] <= midpoint]
                second_half = [row for row in trend_rows if row[0] > midpoint]
                first_p95 = memory_p95(first_half)
                second_p95 = memory_p95(second_half)
                growth_tolerance = max(
                    GROWTH_ABSOLUTE_TOLERANCE_BYTES,
                    int(first_p95 * GROWTH_RELATIVE_TOLERANCE),
                )
                growth_passed = second_p95 <= first_p95 + growth_tolerance

                require(len(rounds) == READ_ROUND_COUNT, "read round count differs from the requested rounds")
                require(all(round_row["usage_items"] == 100 and round_row["error_items"] == 100 for round_row in rounds),
                        "a repeated usage or error query returned an incomplete synthetic page")
                require(all(round_row["trigger_interval_seconds"] >= MIN_READ_ROUND_INTERVAL_SECONDS for round_row in rounds),
                        "read rounds were too close together")
                if implementation == "rust":
                    require(overall["memory_peak_bytes"] <= memory_limit_bytes,
                            "Rust cgroup memory peak exceeded the configured limit")
                    require(growth_passed, "Rust cgroup memory kept growing in the second half")
            finally:
                sampler.close()

        write_requests = sum(native.calls.values())
        mock_summary = {
            "non_read_only_requests": write_requests,
            "unexpected_requests": native.unexpected,
            "active_requests_at_shutdown": native.active,
        }
        require(write_requests == 0, "a non-read-only loopback mock request was observed")
        require(native.unexpected == 0 and native.active == 0, "loopback mock did not settle cleanly")
        require(shutdown.get("clean") is True, "runtime did not shut down cleanly")

    return {
        "implementation": implementation,
        "dataset_counts": dataset,
        "operation_history": {
            **operation_history,
            **operation_result,
            "pending": history["pending"],
            "read_requests": len(operation_latencies),
            "read_latency_ms": distribution(operation_latencies),
        },
        "idle": idle,
        "detection_history": detection_history,
        "detection_log_page_latency_ms": distribution(detection_latencies),
        "snapshot_request_latency_ms": distribution(snapshot_latencies),
        "usage_query_request_latency_ms": distribution(usage_latencies),
        "error_query_request_latency_ms": distribution(error_latencies),
        "read_round_successes": len(rounds),
        "read_rounds": rounds,
        "resource_overall": overall,
        "memory_growth": {
            "first_half_p95_bytes": first_p95,
            "second_half_p95_bytes": second_p95,
            "allowed_increase_bytes": growth_tolerance,
            "passed": growth_passed,
        },
        "rust_memory_limit_bytes": memory_limit_bytes if implementation == "rust" else None,
        "loopback_mocks": mock_summary,
        "shutdown": shutdown,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python-image")
    parser.add_argument("--rust-image")
    parser.add_argument("--implementation", choices=("both", "python", "rust"), default="both")
    parser.add_argument("--postgres-bin-dir")
    parser.add_argument("--max-memory-mib", type=int, default=512)
    args = parser.parse_args()
    require(args.implementation != "both" or bool(args.python_image), "--python-image is required for both implementations")
    require(args.implementation == "rust" or bool(args.python_image), "--python-image is required for Python measurement")
    require(args.implementation == "python" or bool(args.rust_image), "--rust-image is required for Rust measurement")
    require(args.max_memory_mib > 0, "--max-memory-mib must be positive")
    return args


def run(args: argparse.Namespace) -> dict:
    require(platform.system() == "Linux" and platform.machine() in {"x86_64", "amd64"},
            "run this resource regression on Linux x86_64")
    memory_limit_bytes = args.max_memory_mib * 1024 * 1024
    implementations = []
    if args.implementation in {"both", "python"}:
        implementations.append(("python", args.python_image))
    if args.implementation in {"both", "rust"}:
        implementations.append(("rust", args.rust_image))

    report = {
        "status": "failed",
        "scope": "synthetic history memory regression",
        "test_source_sha256": source_sha256(),
        "reference_python_sha": REFERENCE_SHA,
        "synthetic_dataset": {
            "accounts": ACCOUNT_COUNT,
            "usage_rows": USAGE_COUNT,
            "usage_rows_all_stream_samples": True,
            "error_rows": ERROR_COUNT,
            "completed_operation_jobs": OPERATIONS_COUNT,
            "operation_history_bytes": OPERATIONS_TARGET_BYTES,
            "rust_automatic_detection_jobs": DETECTION_COUNT,
            "rust_excluded_manual_jobs": MANUAL_TEST_COUNT,
            "seven_day_window_counts_asserted": True,
        },
        "procedure": {
            "idle_seconds": IDLE_SECONDS,
            "read_rounds_per_runtime": READ_ROUND_COUNT,
            "minimum_read_round_interval_seconds": MIN_READ_ROUND_INTERVAL_SECONDS,
            "rust_memory_peak_limit_bytes": memory_limit_bytes,
            "rust_second_half_growth_allowance": {
                "absolute_bytes": GROWTH_ABSOLUTE_TOLERANCE_BYTES,
                "relative": GROWTH_RELATIVE_TOLERANCE,
            },
            "automatic_recovery": False,
            "daily_test": False,
            "auto_credit_reset": False,
            "bark": False,
            "request_methods": ["GET"],
        },
        "runtimes": [],
    }

    with tempfile.TemporaryDirectory(prefix="server-history-memory-") as directory:
        root = Path(directory)
        reference = archive_reference_app(root / "reference", REFERENCE_SHA)
        postgres = TemporaryPostgres(root, args.postgres_bin_dir)
        try:
            postgres.start()
            dataset = seed_synthetic_dataset(postgres.url)
            report["synthetic_dataset"]["database_counts"] = dataset
            for implementation, image in implementations:
                report["runtimes"].append(measure_runtime(
                    implementation=implementation,
                    image=image,
                    database_url=postgres.url,
                    reference_app=reference,
                    root=root,
                    memory_limit_bytes=memory_limit_bytes,
                ))
        finally:
            postgres.close()

    report["status"] = "passed"
    return report


def main() -> int:
    try:
        args = parse_args()
        report = run(args)
        print(json.dumps(report, ensure_ascii=True, indent=2))
        return 0
    except CheckFailure as error:
        print(json.dumps({"status": "failed", "error": str(error), "test_source_sha256": source_sha256()}))
        return 1
    except Exception as error:
        print(json.dumps({"status": "failed", "error": type(error).__name__, "test_source_sha256": source_sha256()}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
