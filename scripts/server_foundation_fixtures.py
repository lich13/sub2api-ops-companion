#!/usr/bin/env python3
"""Generate synthetic Rust foundation regression fixtures from the pinned Python source."""

from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


from server_reference import REFERENCE_SHA
AT = datetime(2031, 4, 5, 6, 7, 8, tzinfo=timezone.utc)
VECTOR_IDS = {
    "account_template_version_from_operation_version",
    "account_versions_parent_child_identity",
    "config_revision_unicode_null_number_datetime",
    "desktop_account_dto_version_group_order_invariance",
    "model_catalog_account_cache_identity",
    "model_catalog_accounts_routes_revision",
    "model_catalog_final_response_etag",
    "model_catalog_group_version",
    "model_catalog_native_body_revision",
    "model_rules_revision_compact_unicode_null_number",
    "operation_digest_unicode_null_number_datetime",
    "operation_versions_full_synthetic_account",
    "operation_versions_group_order_invariance",
    "operation_versions_model_config_sensitivity",
}


def export_pinned_app(root: Path, repo: Path) -> None:
    listed = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", REFERENCE_SHA, "--", "app"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    paths = [line for line in listed.stdout.splitlines() if line.startswith("app/")]
    if not paths:
        raise RuntimeError(f"no app tree at pinned source {REFERENCE_SHA}")
    for relative in paths:
        output = subprocess.run(
            ["git", "show", f"{REFERENCE_SHA}:{relative}"],
            cwd=repo,
            check=True,
            capture_output=True,
        )
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(output.stdout)


def jsonable(value: Any) -> Any:
    if isinstance(value, datetime):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(key): jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(item) for item in value]
    return value


def projection(summary: dict[str, Any]) -> dict[str, Any]:
    return {
        "account_id": summary["account_id"],
        "score": summary["score"],
        "grade": summary["grade"],
        "reasons": summary["reasons"],
        "sample_status": summary["sample_status"],
        "cap": summary["cap"],
        "consecutive_failures": summary["consecutive_failures"],
        "coverage": summary["coverage"],
        "reliability": {
            key: summary["reliability"].get(key)
            for key in ("successes", "failures", "total", "causes", "mode", "score", "effective_rate")
        },
        "ttft": {
            key: summary["ttft"].get(key)
            for key in ("samples", "compared", "coverage", "score", "p50", "tail", "missing_reason")
        },
        "throughput": {
            key: summary["throughput"].get(key)
            for key in ("samples", "compared", "coverage", "score", "p50", "tail", "missing_reason")
        },
    }


def sample_projection(sample: dict[str, Any] | None) -> dict[str, Any] | None:
    if sample is None:
        return None
    sample = dict(sample)
    sample["cohort"] = list(sample["cohort"])
    return jsonable(sample)


def make_usage_row(account_id: int, row_id: int, created_at: datetime) -> dict[str, Any]:
    return {
        "id": row_id,
        "account_id": account_id,
        "created_at": created_at.isoformat(),
        "stream": True,
        "inbound_endpoint": "/v1/responses",
        "model": "gpt-5.6-luna",
        "upstream_model": "gpt-5.6-luna",
        "input_tokens": 1200,
        "cache_read_tokens": 0,
        "cache_creation_tokens": 0,
        "output_tokens": 100,
        "first_token_ms": 450,
        "duration_ms": 1250,
        "openai_ws_mode": False,
    }


def build_fixtures(repo: Path, app_root: Path) -> dict[str, Any]:
    sys.path.insert(0, str(app_root))
    from app.account_quality import calculate, failure_cause, failures, performance
    from app.atomic_config import write_json
    from app.auto_reset import validate_state
    from app.desktop_usage import project_usage
    from app.oauth_monitor import OAuthStateStore
    from app.quota_snapshot import usage_windows

    vectors = json.loads((repo / "tests/contracts/version-vectors.json").read_text(encoding="utf-8"))
    actual_vector_ids = {vector["id"] for vector in vectors["vectors"]}
    if actual_vector_ids != VECTOR_IDS:
        raise RuntimeError(f"version-vector set changed: {sorted(actual_vector_ids ^ VECTOR_IDS)}")

    oauth_row = {
        "id": 73,
        "platform": "openai",
        "type": "oauth",
        "parent_account_id": None,
        "credentials": {"plan_type": "plus"},
        "extra": {
            "codex_usage_updated_at": AT.isoformat(),
            "codex_5h_used_percent": 105,
            "codex_5h_reset_at": (AT + timedelta(hours=2)).isoformat(),
            "codex_7d_used_percent": 40,
            "codex_7d_reset_at": (AT + timedelta(days=4)).isoformat(),
            "codex_reset_credit_snapshot": {
                "available_count": 4,
                "credits": [
                    {"expires_at": (AT + timedelta(days=1)).isoformat()},
                    {"expires_at": (AT - timedelta(minutes=1)).isoformat()},
                    {"expires_at": (AT + timedelta(days=2)).isoformat()},
                    {"expires_at": (AT + timedelta(days=2)).isoformat()},
                ],
                "fetched_at": AT.isoformat(),
            },
        },
    }
    apikey_row = {
        "id": 74,
        "platform": "openai",
        "type": "apikey",
        "extra": {
            "quota_daily_limit": 10,
            "quota_daily_reset_mode": "fixed",
            "quota_daily_reset_at": (AT + timedelta(days=1)).isoformat(),
            "quota_weekly_limit": 100,
            "quota_weekly_used": 25,
            "quota_weekly_start": (AT - timedelta(days=1)).isoformat(),
        },
    }
    usage = {
        "oauth_row": oauth_row,
        "oauth_expected_windows": jsonable(usage_windows(oauth_row, AT, None)),
        "oauth_expected_projection": jsonable(project_usage(oauth_row, AT, None)),
        "apikey_row": apikey_row,
        "apikey_expected_projection": jsonable(project_usage(apikey_row, AT, None)),
    }

    accounts = [
        {"id": 73, "platform": "openai", "type": "oauth", "deleted_at": None},
        {"id": 74, "platform": "openai", "type": "apikey", "deleted_at": None},
        {"id": 75, "platform": "anthropic", "type": "oauth", "deleted_at": None},
        {"id": 76, "platform": "openai", "type": "oauth", "deleted_at": "2031-04-05T06:07:08+00:00"},
    ]
    quality_usage: list[dict[str, Any]] = []
    for account_id, start_id in ((73, 73000), (74, 74000)):
        rows = [make_usage_row(account_id, start_id + index, AT - timedelta(minutes=30, seconds=index)) for index in range(25)]
        quality_usage.extend(rows)
        if account_id == 73:
            quality_usage.append(copy.deepcopy(rows[0]))
    quality_usage.extend(
        [
            make_usage_row(75, 75000, AT - timedelta(minutes=2)),
            make_usage_row(76, 76000, AT - timedelta(minutes=2)),
        ]
    )

    error_cases = [
        {
            "name": "auth",
            "row": {
                "id": 9101,
                "account_id": 73,
                "request_id": "synthetic-auth-1",
                "created_at": (AT - timedelta(minutes=5)).isoformat(),
                "error_owner": "provider",
                "error_phase": "upstream",
                "upstream_status_code": 401,
                "message": "invalid_api_key",
            },
        },
        {
            "name": "generic_rate_limit",
            "row": {
                "id": 9102,
                "account_id": 73,
                "request_id": "synthetic-rate-1",
                "created_at": (AT - timedelta(minutes=4)).isoformat(),
                "error_owner": "provider",
                "error_phase": "upstream",
                "upstream_status_code": 429,
                "message": "gateway temporarily overloaded",
            },
        },
        {
            "name": "quota_excluded",
            "row": {
                "id": 9103,
                "account_id": 73,
                "request_id": "synthetic-quota-1",
                "created_at": (AT - timedelta(minutes=3)).isoformat(),
                "error_owner": "provider",
                "error_phase": "upstream",
                "upstream_status_code": 429,
                "message": "usage_limit_reached",
            },
        },
        {
            "name": "local_concurrency_excluded",
            "row": {
                "id": 9104,
                "account_id": 73,
                "request_id": "synthetic-local-1",
                "created_at": (AT - timedelta(minutes=2)).isoformat(),
                "error_owner": "client",
                "error_phase": "concurrency",
                "upstream_status_code": 429,
                "message": "Too many concurrent requests for API key",
            },
        },
        {
            "name": "client_cancelled_excluded",
            "row": {
                "id": 9105,
                "account_id": 73,
                "request_id": "synthetic-cancel-1",
                "created_at": (AT - timedelta(minutes=1)).isoformat(),
                "error_owner": "provider",
                "error_phase": "upstream",
                "upstream_status_code": 499,
                "message": "client cancelled request",
            },
        },
        {
            "name": "content_policy_excluded",
            "row": {
                "id": 9106,
                "account_id": 73,
                "request_id": "synthetic-policy-1",
                "created_at": (AT - timedelta(seconds=30)).isoformat(),
                "error_owner": "provider",
                "error_phase": "upstream",
                "upstream_status_code": 400,
                "message": "content policy violation",
            },
        },
    ]
    error_rows = [case["row"] for case in error_cases]
    expected_failure_rows = failures(error_rows, {account["id"]: account for account in accounts}, AT)
    quality_summaries, _ = calculate(accounts, quality_usage, error_rows, AT)
    performance_account = accounts[0]
    performance_base = quality_usage[0]
    missing_cache_counter = dict(performance_base)
    missing_cache_counter.pop("cache_read_tokens")
    image_excluded = {**performance_base, "image_count": 1}
    performance_cases = [
        {"name": "valid", "row": performance_base},
        {"name": "missing_cache_counter", "row": missing_cache_counter},
        {"name": "image_excluded", "row": image_excluded},
    ]
    quality = {
        "accounts": accounts,
        "usage_rows": quality_usage,
        "failure_cases": [
            {"name": case["name"], "row": case["row"], "expected_cause": failure_cause(case["row"])}
            for case in error_cases
        ],
        "expected_failures": [
            {
                "account_id": item["account_id"],
                "at": jsonable(item["at"]),
                "cause": item["cause"],
                "request_known": item["request_known"],
            }
            for item in expected_failure_rows
        ],
        "performance_cases": [
            {
                "name": case["name"],
                "row": case["row"],
                "expected": sample_projection(performance(case["row"], performance_account)),
            }
            for case in performance_cases
        ],
        "expected_summaries": {
            str(account_id): projection(summary)
            for account_id, summary in quality_summaries.items()
        },
        "excluded_account_ids": [75, 76],
    }

    valid_credit_state = {
        "stage": "waiting",
        "episode": "synthetic-credit-episode",
        "owns_pause": True,
        "attempt_at": AT.isoformat(),
        "policy_version": 1,
        "test_attempts": 0,
        "policy_control_generation": 2,
        "policy_method_generation": 1,
        "outcome": "restored",
        "policy_detection_version": "a" * 64,
    }
    invalid_credit_states = []
    for name, key, invalid_value in (
        ("stage", "stage", "unrecognized"),
        ("timestamp", "attempt_at", "not-a-time"),
        ("pause_ownership", "owns_pause", "true"),
        ("policy_version", "policy_version", 2),
        ("control_generation", "policy_control_generation", -1),
        ("outcome", "outcome", "surprised"),
        ("detection_revision", "policy_detection_version", "g" * 64),
    ):
        state = copy.deepcopy(valid_credit_state)
        state[key] = invalid_value
        try:
            validate_state(state)
        except ValueError as error:
            expected_error = str(error)
        else:
            expected_error = None
        invalid_credit_states.append({"name": name, "state": state, "expected_error": expected_error})

    manual_row = {
        "id": 73,
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "concurrency": 4,
        "extra": {},
    }
    with tempfile.TemporaryDirectory(prefix="sub2ops-foundation-manual-") as temporary:
        path = Path(temporary) / "oauth-state.json"
        state_store = OAuthStateStore(str(path))

        def seed_pending(data: dict[str, Any]) -> None:
            data["pending_events"].update(
                {
                    "keep-active": {"account_id": 73, "stage": "active_usage"},
                    "drop-ready": {"account_id": 73, "stage": "ready"},
                    "keep-other": {"account_id": 74, "stage": "ready"},
                }
            )

        state_store.transaction(seed_pending)
        generation = state_store.manual_control(manual_row, False, AT)
        state_store.commit(
            results={73: {"template_type": "oauth", "stale_result": True}},
            scheduler_updates={
                73: {
                    "manual_control": {"generation": 99, "enabled": True, "at": AT.isoformat()},
                    "stale_scheduler_marker": True,
                }
            },
            pending_events={"stale-ready": {"account_id": 73, "stage": "ready"}},
            expected_generations={73: 0},
        )
        state_snapshot = state_store.snapshot()
        manual_control = state_snapshot["scheduler"]["73"]["manual_control"]
        manual_control_fixture = {
            "row": manual_row,
            "generation": generation,
            "expected_generation": 1,
            "expected_control": jsonable(manual_control),
            "expected_stale_marker": state_snapshot["scheduler"]["73"].get("stale_scheduler_marker"),
            "expected_oauth_result": state_snapshot["oauth_results"].get("73"),
            "expected_pending_stages": {
                key: value["stage"] for key, value in sorted(state_snapshot["pending_events"].items())
            },
        }

    with tempfile.TemporaryDirectory(prefix="sub2ops-foundation-write-") as temporary:
        root = Path(temporary)
        target = root / "blocked-target"
        target.mkdir()
        marker = target / "marker.txt"
        marker.write_text("synthetic-preserved", encoding="utf-8")
        try:
            write_json(target, {"synthetic": True})
        except OSError as error:
            exception = type(error).__name__
        else:
            exception = None
        atomic_failure = {
            "target_is_directory": target.is_dir(),
            "reference_exception": exception,
            "marker_after": marker.read_text(encoding="utf-8"),
            "temporary_artifacts_after": sorted(item.name for item in root.iterdir() if item.name.startswith(".blocked-target.")),
        }

    with tempfile.TemporaryDirectory(prefix="sub2ops-foundation-lost-update-") as temporary:
        path = Path(temporary) / "state.json"
        first = OAuthStateStore(str(path))
        second = OAuthStateStore(str(path))
        first.transaction(lambda data: data["scheduler"].setdefault("73", {}).__setitem__("first", True))
        second.transaction(lambda data: data["scheduler"].setdefault("73", {}).__setitem__("second", True))
        expected_lost_update = first.snapshot()["scheduler"]["73"]

    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "synthetic_only": True,
        "version_vector_ids": sorted(actual_vector_ids),
        "at": AT.isoformat(),
        "usage": usage,
        "quality": quality,
        "credit_state": {
            "valid_state": valid_credit_state,
            "valid_expected_error": None,
            "invalid_states": invalid_credit_states,
            "manual_generation": manual_control_fixture,
        },
        "persistence": {
            "atomic_write_failure": atomic_failure,
            "lost_update_expected_scheduler": expected_lost_update,
            "read_only_absent_state_expectation": {
                "initial_path_exists": False,
                "snapshot_must_not_create_parent": True,
                "transaction_error_code": "read_only",
            },
        },
    }


def main() -> None:
    repo = Path(__file__).resolve().parents[1]
    output = repo / "tests/contracts/foundation-fixtures.json"
    with tempfile.TemporaryDirectory(prefix="sub2ops-foundation-app-") as temporary:
        app_root = Path(temporary)
        export_pinned_app(app_root, repo)
        fixture = build_fixtures(repo, app_root)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {output.relative_to(repo)} from {REFERENCE_SHA}")


if __name__ == "__main__":
    main()
