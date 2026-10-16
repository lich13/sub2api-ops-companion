#!/usr/bin/env python3
"""Generate synthetic recovery contracts from the pinned 0.1.39 Python source."""

from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any


from server_reference import REFERENCE_SHA
AT = datetime(2031, 4, 5, 6, 7, 8, tzinfo=timezone.utc)
DAY_START = datetime(2031, 4, 5, 20, 59, 0, tzinfo=timezone.utc)
DAY_TARGET = datetime(2031, 4, 5, 21, 0, 0, tzinfo=timezone.utc)
ACCESS_EXAMPLE = "example.invalid"
REFRESH_EXAMPLE = "refresh.example.invalid"


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
        content = subprocess.run(
            ["git", "show", f"{REFERENCE_SHA}:{relative}"],
            cwd=repo,
            check=True,
            capture_output=True,
        )
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content.stdout)


def output_value(value: Any) -> Any:
    if isinstance(value, datetime):
        return value.astimezone(timezone.utc).isoformat()
    if isinstance(value, dict):
        return {str(key): output_value(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set)):
        values = sorted(value) if isinstance(value, set) else value
        return [output_value(item) for item in values]
    return value


def stamp(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat()


def account(account_id: int = 73, **overrides: Any) -> dict[str, Any]:
    row = {
        "id": account_id,
        "platform": "openai",
        "type": "oauth",
        "parent_account_id": None,
        "credentials": {
            "plan_type": "plus",
            "access_token": ACCESS_EXAMPLE,
            "refresh_token": REFRESH_EXAMPLE,
            "id_token": None,
        },
        "extra": {},
        "deleted_at": None,
        "status": "active",
        "schedulable": True,
        "concurrency": 1,
        "rate_limited_at": None,
        "rate_limit_reset_at": None,
        "overload_until": None,
        "temp_unschedulable_until": None,
        "temp_unschedulable_reason": None,
        "expires_at": None,
        "auto_pause_on_expired": False,
    }
    row.update(overrides)
    return row


def summary(
    *,
    used_5h: float = 10,
    used_7d: float = 20,
    reset_5h: datetime | None = None,
    reset_7d: datetime | None = None,
    source_5h: str = "server_exact",
    source_7d: str = "server_exact",
    plan_type: str = "plus",
    updated_at: datetime = AT,
) -> dict[str, Any]:
    return {
        "plan_type": plan_type,
        "updated_at": stamp(updated_at),
        "ui_windows": [
            {
                "key": "codex_5h",
                "label": "5h",
                "used_percent": used_5h,
                "reset_at": stamp(reset_5h or (updated_at + timedelta(hours=2))),
                "reset_source": source_5h,
                "window_minutes": 300,
            },
            {
                "key": "codex_7d",
                "label": "7d",
                "used_percent": used_7d,
                "reset_at": stamp(reset_7d or (updated_at + timedelta(days=3))),
                "reset_source": source_7d,
                "window_minutes": 10080,
            },
        ],
    }


def quota_result(
    queried_at: datetime = AT,
    *,
    used_5h: float = 10,
    used_7d: float = 20,
    reset_5h: datetime | None = None,
    reset_7d: datetime | None = None,
    source_5h: str = "server_exact",
    source_7d: str = "server_exact",
) -> dict[str, Any]:
    return {
        "success": True,
        "queried_at": stamp(queried_at),
        "source": "sub2api_admin_usage",
        "oauth_quota": summary(
            used_5h=used_5h,
            used_7d=used_7d,
            reset_5h=reset_5h,
            reset_7d=reset_7d,
            source_5h=source_5h,
            source_7d=source_7d,
            updated_at=queried_at,
        ),
    }


def recovery_intent(status: str = "pending", **values: Any) -> dict[str, Any]:
    intent = {
        "status": status,
        "due_at": stamp(AT - timedelta(minutes=5)),
        "fingerprint": "fixture-reset-boundary",
    }
    intent.update(values)
    return intent


class MemoryScheduleStore:
    def __init__(self) -> None:
        self.data: dict[str, Any] = {"daily_test": {"batches": {}}}

    def cached_snapshot(self) -> dict[str, Any]:
        return copy.deepcopy(self.data)

    def commit(self, **values: Any) -> None:
        self.data.update(copy.deepcopy(values))


def build_fixtures(repo: Path, app_root: Path) -> dict[str, Any]:
    sys.path.insert(0, str(app_root))
    from app import bark as py_bark
    from app.daily_test import DailyTestSchedule, daily_account_eligible, next_daily_time
    from app.key_fallback import (
        AVAILABLE,
        UNKNOWN,
        UNAVAILABLE,
        classify_grok_oauth_account,
        classify_oauth_account,
        desired_key_schedulable,
    )
    from app.oauth_monitor import (
        OAuthStateStore,
        _quota_recovery_descriptor,
        _rate_limit_recovery_descriptor,
        _threshold_quota_available,
        _threshold_recovery_descriptor,
        account_recovery_confirmed,
        automatic_recovery_eligible,
        build_monitor_candidates,
        control_allows_recovery,
        recovery_block_change_is_safe,
        recovery_block_signature,
        recovery_quota_ready,
        _threshold_reason_details,
    )
    from app.oauth_queries import (
        AUTO_QUERY_LIMIT,
        AUTO_WINDOW_SECONDS,
        MIN_QUERY_SECONDS,
        automatic_gate,
        credential_fingerprint,
        fresh_quota,
        query_metadata,
    )
    from app.recovery_policy import CONNECTION_IDS, MODEL_IDS, account_ids, eligible_ids, recovery_method
    from app.reset_credit_observation import (
        available_credits,
        consumption_result,
        create_observation,
        digest,
        limit_fingerprint,
        normalized_credits,
        observation_from_usage,
        receipt_diagnostics,
    )

    admission_row = account()
    fresh_result = quota_result(
        AT,
        reset_5h=AT + timedelta(hours=1),
        reset_7d=AT + timedelta(days=3),
    )
    fresh_vectors = [
        ("now", quota_result(AT, reset_5h=AT + timedelta(hours=1), reset_7d=AT + timedelta(days=3)), AT),
        (
            "exactly_one_hour",
            quota_result(AT - timedelta(seconds=MIN_QUERY_SECONDS), reset_5h=AT + timedelta(hours=1), reset_7d=AT + timedelta(days=3)),
            AT,
        ),
        (
            "one_second_too_old",
            quota_result(AT - timedelta(seconds=MIN_QUERY_SECONDS + 1), reset_5h=AT + timedelta(hours=1), reset_7d=AT + timedelta(days=3)),
            AT,
        ),
        (
            "future_observation",
            quota_result(AT + timedelta(seconds=1), reset_5h=AT + timedelta(hours=1), reset_7d=AT + timedelta(days=3)),
            AT,
        ),
        (
            "reset_boundary_now",
            quota_result(AT, reset_5h=AT, reset_7d=AT + timedelta(days=3)),
            AT,
        ),
    ]
    fresh_quota_vectors = []
    for name, result, now in fresh_vectors:
        fresh_quota_vectors.append({
            "name": name,
            "row": admission_row,
            "result": result,
            "now": stamp(now),
            "expected": fresh_quota(admission_row, result, now),
        })

    def attempts(*hours_ago: int) -> list[str]:
        return [stamp(AT - timedelta(hours=hours)) for hours in hours_ago]

    gate_inputs = [
        {
            "name": "cooldown_one_second_remaining",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"last_query_at": stamp(AT - timedelta(seconds=3599))}},
            "wait_for_reset": False,
            "bypass_interval": False,
            "required_budget": 1,
        },
        {
            "name": "cooldown_exact_boundary",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"last_query_at": stamp(AT - timedelta(seconds=3600))}},
            "wait_for_reset": False,
            "bypass_interval": False,
            "required_budget": 1,
        },
        {
            "name": "automatic_budget_six_plus_two",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"automatic_attempts": attempts(23, 22, 21, 20, 19)}},
            "wait_for_reset": False,
            "bypass_interval": True,
            "required_budget": 2,
        },
        {
            "name": "automatic_budget_exactly_six",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"automatic_attempts": attempts(23, 22, 21, 20)}},
            "wait_for_reset": False,
            "bypass_interval": True,
            "required_budget": 2,
        },
        {
            "name": "attempt_at_24_hour_boundary_is_expired",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"automatic_attempts": [stamp(AT - timedelta(seconds=AUTO_WINDOW_SECONDS)), *attempts(23, 22, 21, 20)]}},
            "wait_for_reset": False,
            "bypass_interval": True,
            "required_budget": 2,
        },
        {
            "name": "query_backoff",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"retry_at": stamp(AT + timedelta(seconds=90))}},
            "wait_for_reset": False,
            "bypass_interval": True,
            "required_budget": 1,
        },
        {
            "name": "natural_reset_wait",
            "row": account(rate_limit_reset_at=stamp(AT + timedelta(seconds=1))),
            "saved": None,
            "metadata": {},
            "wait_for_reset": True,
            "bypass_interval": True,
            "required_budget": 1,
        },
        {
            "name": "same_credential_auth_error_pauses",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"auth_fingerprint": credential_fingerprint(admission_row)}},
            "wait_for_reset": False,
            "bypass_interval": True,
            "required_budget": 1,
        },
        {
            "name": "updated_credential_clears_auth_pause",
            "row": admission_row,
            "saved": None,
            "metadata": {"quota_query": {"auth_fingerprint": "0" * 64}},
            "wait_for_reset": False,
            "bypass_interval": True,
            "required_budget": 1,
        },
    ]
    gate_vectors = []
    for case in gate_inputs:
        reason, deadline = automatic_gate(
            case["row"],
            case["saved"],
            case["metadata"],
            AT,
            wait_for_reset=case["wait_for_reset"],
            bypass_interval=case["bypass_interval"],
            required_budget=case["required_budget"],
        )
        gate_vectors.append({**case, "expected_reason": reason, "expected_deadline": deadline})

    selection_rows = [
        account(73),
        account(74, parent_account_id=73),
        account(75, extra={"parent_account_id": 73}),
        account(76, deleted_at=stamp(AT)),
        account(77, platform="grok"),
    ]
    policy_config_cases = [
        {"name": "legacy_selection", "config": {}, "id": 99},
        {"name": "explicit_model", "config": {CONNECTION_IDS: [74, 73], MODEL_IDS: [75]}, "id": 75},
        {"name": "explicit_connection", "config": {CONNECTION_IDS: [74, 73], MODEL_IDS: [75]}, "id": 73},
        {"name": "explicit_empty", "config": {CONNECTION_IDS: [], MODEL_IDS: []}, "id": 73},
        {"name": "model_precedes_connection", "config": {CONNECTION_IDS: [73], MODEL_IDS: [73]}, "id": 73},
    ]
    policy_methods = []
    for case in policy_config_cases:
        settings = SimpleNamespace(**case["config"])
        policy_methods.append({
            **case,
            "expected_method": recovery_method(settings, case["id"]),
        })

    threshold_payload = {
        "source": "account_scheduling_threshold",
        "platform": "openai",
        "window": "codex_5h",
        "threshold_percent": 80,
        "until_unix": stamp(AT + timedelta(hours=2)),
    }
    threshold_reason = json.dumps(threshold_payload, ensure_ascii=False, separators=(",", ":"))
    threshold_row = account(
        temp_unschedulable_until=threshold_payload["until_unix"],
        temp_unschedulable_reason=threshold_reason,
    )
    threshold_summary = summary(used_5h=79, used_7d=20)
    depleted_summary = summary(
        used_5h=100,
        used_7d=100,
        reset_5h=AT + timedelta(hours=2),
        reset_7d=AT + timedelta(days=3),
        source_7d="estimated_from_remaining",
    )
    rate_row = account(
        rate_limited_at=stamp(AT - timedelta(hours=2)),
        rate_limit_reset_at=stamp(AT - timedelta(hours=1)),
    )
    block_before = account(
        rate_limited_at=stamp(AT - timedelta(hours=2)),
        rate_limit_reset_at=stamp(AT - timedelta(hours=1)),
        extra={"model_rate_limits": {"fixture/model": {"remaining": 0}}},
    )
    block_after_cleared = {
        **block_before,
        "rate_limited_at": None,
        "rate_limit_reset_at": None,
        "extra": {"model_rate_limits": {}},
    }
    block_after_changed = {**block_before, "status": "disabled"}

    rule_vectors = {
        "signature": {
            "row": block_before,
            "expected": recovery_block_signature(block_before),
        },
        "change_safety": [
            {"name": "cleared_block_is_safe", "before": block_before, "after": block_after_cleared,
             "expected": recovery_block_change_is_safe(block_before, block_after_cleared)},
            {"name": "status_change_is_unsafe", "before": block_before, "after": block_after_changed,
             "expected": recovery_block_change_is_safe(block_before, block_after_changed)},
        ],
        "threshold": {
            "value": threshold_reason,
            "expected": _threshold_reason_details(threshold_reason),
            "row": threshold_row,
            "windows": ["codex_5h"],
            "eligibility_expected": automatic_recovery_eligible(
                threshold_row, exhausted_window_keys=["codex_5h"], now=AT
            ),
            "unobserved_window_expected": automatic_recovery_eligible(
                threshold_row, exhausted_window_keys=[], now=AT
            ),
            "descriptor_summary": threshold_summary,
            "descriptor_expected": _threshold_recovery_descriptor(threshold_row, threshold_summary),
            "quota_available_expected": _threshold_quota_available(
                threshold_summary,
                _threshold_recovery_descriptor(threshold_row, threshold_summary) or {},
            ),
        },
        "confirmed": {
            "row": account(),
            "expected": account_recovery_confirmed(account()),
        },
        "quota_descriptor": {
            "summary": depleted_summary,
            "at": stamp(AT),
            "expected": _quota_recovery_descriptor(depleted_summary, AT),
        },
        "rate_descriptor": {
            "row": rate_row,
            "summary": threshold_summary,
            "at": stamp(AT),
            "expected": _rate_limit_recovery_descriptor(rate_row, threshold_summary, AT),
        },
        "retry_at": [
            {"attempt": attempt, "expected": (AT + timedelta(seconds=[60, 300, 900, 1800][min(max(0, attempt - 1), 3)])).isoformat()}
            for attempt in [0, 1, 2, 4, 5]
        ],
    }

    ready_result = quota_result(
        AT - timedelta(minutes=5),
        reset_5h=AT + timedelta(hours=1),
        reset_7d=AT + timedelta(days=3),
    )
    ready_row = account(rate_limit_reset_at=stamp(AT - timedelta(minutes=10)))
    ready_metadata = {
        "recovery_intent": {"due_at": stamp(AT - timedelta(minutes=20))},
        "last_error_at": stamp(AT - timedelta(minutes=30)),
        "last_error_code": "synthetic_failure",
    }
    readiness_vectors = []
    for name, row, result, metadata in [
        ("after_natural_reset", ready_row, ready_result, ready_metadata),
        ("before_natural_reset", account(rate_limit_reset_at=stamp(AT - timedelta(minutes=4))), ready_result, ready_metadata),
        ("stale_observation", ready_row, quota_result(AT - timedelta(hours=1, seconds=1)), ready_metadata),
        ("error_after_observation", ready_row, ready_result,
         {**ready_metadata, "last_error_at": stamp(AT - timedelta(minutes=4)), "last_error_code": "synthetic_failure"}),
    ]:
        readiness_vectors.append({
            "name": name,
            "row": row,
            "result": result,
            "metadata": metadata,
            "now": stamp(AT),
            "expected": recovery_quota_ready(row, result, metadata, AT),
        })

    control_vectors = []
    for name, row, result, metadata in [
        ("no_manual_control", ready_row, ready_result, {}),
        ("manual_disabled", {**ready_row, "schedulable": False}, ready_result,
         {"manual_control": {"generation": 3, "at": stamp(AT - timedelta(minutes=10)), "enabled": False}}),
        ("observation_at_manual_change", ready_row,
         quota_result(AT - timedelta(minutes=10)),
         {"manual_control": {"generation": 4, "at": stamp(AT - timedelta(minutes=10)), "enabled": True}}),
        ("fresh_observation_after_manual_change", ready_row, ready_result,
         {"manual_control": {"generation": 5, "at": stamp(AT - timedelta(minutes=10)),
                             "evidence_at": stamp(AT - timedelta(minutes=9)), "enabled": True}}),
        ("new_rate_block_after_manual_change", {**ready_row, "rate_limited_at": stamp(AT - timedelta(minutes=5))},
         quota_result(AT - timedelta(hours=2)),
         {"manual_control": {"generation": 6, "at": stamp(AT - timedelta(minutes=10)), "enabled": True}}),
    ]:
        control_vectors.append({
            "name": name,
            "row": row,
            "result": result,
            "metadata": metadata,
            "now": stamp(AT),
            "expected": control_allows_recovery(row, result, metadata, AT),
        })

    candidate_rows = [
        account(73, rate_limited_at=stamp(AT - timedelta(hours=2)), rate_limit_reset_at=stamp(AT - timedelta(minutes=10))),
        account(74, rate_limited_at=stamp(AT - timedelta(hours=2)), rate_limit_reset_at=stamp(AT - timedelta(minutes=10))),
        account(75, rate_limited_at=stamp(AT - timedelta(hours=2)), rate_limit_reset_at=stamp(AT - timedelta(minutes=10))),
        account(76),
        account(77),
    ]
    candidate_results = {
        73: quota_result(AT - timedelta(minutes=5)),
        74: quota_result(AT - timedelta(minutes=5)),
        75: quota_result(AT - timedelta(minutes=5)),
        76: quota_result(AT - timedelta(seconds=AUTO_WINDOW_SECONDS + 100), used_5h=100,
                         reset_5h=AT - timedelta(seconds=30), reset_7d=AT + timedelta(days=2)),
        77: quota_result(AT - timedelta(seconds=AUTO_WINDOW_SECONDS + 100)),
    }
    candidate_scheduler = {
        "73": {"recovery_intent": recovery_intent("pending")},
        "74": {"recovery_intent": recovery_intent("pending", verification_dispatched=True)},
        "75": {"recovery_intent": recovery_intent("blocked", verification_dispatched=True,
                verification_receipt={"result": {}, "block_state": {}, "credential_fingerprint": "0" * 64})},
        "76": {"quota_query": {"auth_fingerprint": "0" * 64}},
        "77": {"recovery_intent": recovery_intent("ready", due_at=stamp(AT - timedelta(minutes=1)),
                verification_dispatched=True, verification_receipt={
                    "result": {}, "block_state": {}, "credential_fingerprint": "0" * 64})},
    }
    candidate_result = build_monitor_candidates(candidate_rows, candidate_results, candidate_scheduler, AT)
    candidate_projection = [
        {
            "account_id": item["account_id"],
            "reason": item["reason"],
            "priority": item["priority"],
            "exact_fingerprint": item["exact_fingerprint"],
            "reuse_quota": item["reuse_quota"] is not None,
        }
        for item in candidate_result
    ]

    daily_vectors: dict[str, Any] = {
        "times": [
            {"name": "one_minute_before", "now": stamp(DAY_START), "time": "05:00",
             "expected": next_daily_time(DAY_START, "05:00")},
            {"name": "exactly_due_moves_to_tomorrow", "now": stamp(DAY_TARGET), "time": "05:00",
             "expected": next_daily_time(DAY_TARGET, "05:00")},
        ],
        "eligibility": [],
    }
    for name, row in [
        ("eligible", account()),
        ("manual_reason", account(temp_unschedulable_reason="人工手动暂停")),
        ("future_reset", account(rate_limit_reset_at=stamp(AT + timedelta(seconds=1)))),
        ("expired_auto_pause", account(auto_pause_on_expired=True, expires_at=stamp(AT))),
        ("rate_limited_without_reset", account(rate_limited_at=stamp(AT - timedelta(minutes=1)))),
    ]:
        daily_vectors["eligibility"].append({
            "name": name,
            "row": row,
            "now": stamp(AT),
            "expected": daily_account_eligible(row, AT),
        })

    settings = SimpleNamespace(oauth_daily_test_enabled=True, oauth_daily_test_time="05:00")
    on_time_store = MemoryScheduleStore()
    on_time_schedule = DailyTestSchedule(settings, on_time_store, DAY_START)
    initial_state = on_time_store.cached_snapshot()["daily_test"]
    on_time_schedule.tick(DAY_TARGET + timedelta(seconds=30))
    queued_state = on_time_store.cached_snapshot()["daily_test"]
    settings.oauth_daily_test_time = "06:30"
    on_time_schedule.tick(DAY_TARGET + timedelta(seconds=45))
    changed_config_state = on_time_store.cached_snapshot()["daily_test"]
    claimed = on_time_schedule.claim(DAY_TARGET + timedelta(seconds=50))
    claimed_state = on_time_store.cached_snapshot()["daily_test"]
    restart_at = DAY_TARGET + timedelta(minutes=10)
    restarted_schedule = DailyTestSchedule(settings, on_time_store, restart_at)
    interrupted_state = on_time_store.cached_snapshot()["daily_test"]

    late_store = MemoryScheduleStore()
    late_settings = SimpleNamespace(oauth_daily_test_enabled=True, oauth_daily_test_time="05:00")
    late_schedule = DailyTestSchedule(late_settings, late_store, DAY_START)
    late_schedule.tick(DAY_TARGET + timedelta(minutes=1))
    daily_vectors["schedule"] = {
        "initial": initial_state,
        "on_time_tick": {"at": stamp(DAY_TARGET + timedelta(seconds=30)), "expected": queued_state},
        "configuration_change": {"at": stamp(DAY_TARGET + timedelta(seconds=45)), "time": "06:30",
                                  "expected": changed_config_state},
        "claim": {"at": stamp(DAY_TARGET + timedelta(seconds=50)), "expected_batch": claimed,
                  "expected_state": claimed_state},
        "restart": {"at": stamp(restart_at), "expected_state": interrupted_state},
        "late_missed": {"at": stamp(DAY_TARGET + timedelta(minutes=1)),
                        "expected_state": late_store.cached_snapshot()["daily_test"]},
    }

    raw_credits = {
        "available_count": 3,
        "credits": [
            {"id": "fixture-expired", "expires_at": stamp(AT - timedelta(minutes=1))},
            {"id": "fixture-boundary", "expires_at": stamp(AT)},
            {"id": "fixture-future", "expires_at": stamp(AT + timedelta(hours=1))},
        ],
        "upstream_note": "ignored synthetic field",
    }
    content = normalized_credits(raw_credits)
    reset_row_before = account()
    reset_row_after = account(extra={"codex_reset_credit_snapshot": raw_credits})
    reset_result = quota_result(
        AT,
        reset_5h=AT + timedelta(hours=1),
        reset_7d=AT + timedelta(days=7),
    )
    reset_result["data"] = {
        "fetched_at": stamp(AT),
        "cache_persisted": True,
        "rate_limit_reset_credits": raw_credits,
    }
    reset_result["success"] = True
    accepted_observation = observation_from_usage(
        reset_row_before, reset_row_after, reset_result, AT - timedelta(seconds=1), AT + timedelta(seconds=1)
    )
    reset_vectors = {
        "raw_credits": raw_credits,
        "normalized": content,
        "normalized_expected": content,
        "available_at_now": available_credits(raw_credits, AT),
        "observation": {
            "row": reset_row_after,
            "content": content,
            "observed_at": stamp(AT),
            "credential_fingerprint": credential_fingerprint(reset_row_after),
            "reset_at": stamp(AT + timedelta(days=7)),
            "window_minutes": 10080,
            "expected": create_observation(
                reset_row_after, content, AT, credential_fingerprint(reset_row_after),
                AT + timedelta(days=7), 10080,
            ),
        },
        "observation_from_usage": {
            "before": reset_row_before,
            "after": reset_row_after,
            "result": reset_result,
            "started": stamp(AT - timedelta(seconds=1)),
            "completed": stamp(AT + timedelta(seconds=1)),
            "expected": accepted_observation,
        },
        "observation_rejections": [],
        "diagnostics": [],
        "consumption": [],
        "digest": digest(content),
        "limit_fingerprint_row": account(
            rate_limited_at=stamp(AT - timedelta(minutes=30)),
            rate_limit_reset_at=stamp(AT + timedelta(minutes=30)),
        ),
        "limit_fingerprint": limit_fingerprint(account(
            rate_limited_at=stamp(AT - timedelta(minutes=30)),
            rate_limit_reset_at=stamp(AT + timedelta(minutes=30)),
        )),
    }
    rejection_cases = []
    for name, before, after, result, started, completed in [
        ("cache_not_persisted", reset_row_before, reset_row_after,
         {**reset_result, "data": {**reset_result["data"], "cache_persisted": False}},
         AT - timedelta(seconds=1), AT + timedelta(seconds=1)),
        ("credential_changed", reset_row_before,
         {**reset_row_after, "credentials": {**reset_row_after["credentials"], "access_token": "changed.example.invalid"}},
         reset_result, AT - timedelta(seconds=1), AT + timedelta(seconds=1)),
        ("limit_changed", reset_row_before,
         {**reset_row_after, "rate_limit_reset_at": stamp(AT + timedelta(minutes=1))},
         reset_result, AT - timedelta(seconds=1), AT + timedelta(seconds=1)),
        ("snapshot_mismatch", reset_row_before,
         {**reset_row_after, "extra": {"codex_reset_credit_snapshot": {"available_count": 0, "credits": []}}},
         reset_result, AT - timedelta(seconds=1), AT + timedelta(seconds=1)),
        ("fetched_before_start_window", reset_row_before, reset_row_after,
         {**reset_result, "data": {**reset_result["data"], "fetched_at": stamp(AT - timedelta(seconds=3))}},
         AT - timedelta(seconds=1), AT + timedelta(seconds=1)),
        ("reset_not_after_fetch", reset_row_before, reset_row_after,
         {**reset_result, "oauth_quota": summary(reset_5h=AT + timedelta(hours=1), reset_7d=AT)},
         AT - timedelta(seconds=1), AT + timedelta(seconds=1)),
    ]:
        rejection_cases.append({
            "name": name,
            "before": before,
            "after": after,
            "result": result,
            "started": stamp(started),
            "completed": stamp(completed),
            "expected": observation_from_usage(before, after, result, started, completed),
        })
    reset_vectors["observation_rejections"] = rejection_cases
    for name, status, data in [
        ("generic_success", 200, {"code": " SUCCESS ", "windows_reset": 1, "cache_persisted": True}),
        ("unknown_code_is_redacted", 200, {"code": "unrecognized fixture code", "windows_reset": 0}),
        ("namespaced_code", 429, {"code": "OPENAI_RATE_LIMIT", "account_state_recovered": False}),
    ]:
        reset_vectors["diagnostics"].append({
            "name": name,
            "status": status,
            "data": data,
            "expected": receipt_diagnostics(status, data),
        })
    for name, data in [
        ("consumed", {"code": "reset", "windows_reset": 1, "credit": {"status": "redeemed"}}),
        ("available_no_credit", {"code": "no_credit", "windows_reset": 0, "credit": {"status": "available"}}),
        ("expired_no_credit", {"code": "nothing_to_reset", "windows_reset": 0, "credit": {"status": "expired"}}),
        ("already_redeemed_is_uncertain", {"code": "already_redeemed", "windows_reset": 0}),
        ("unknown_receipt_is_uncertain", {"code": "fixture_pending", "windows_reset": 0}),
    ]:
        reset_vectors["consumption"].append({
            "name": name,
            "data": data,
            "expected": consumption_result(data),
        })

    bark_error_event = {
        "status": "test_failed",
        "account_id": 73,
        "account_name": "fixture-account",
        "plan_type": "plus",
        "window_labels": ["5h", "7d"],
        "model_id": "example/model",
        "error_code": "quota_wait",
        "error": "synthetic quota report",
    }
    bark_recovered_event = {
        "status": "recovered",
        "test_success": True,
        "account_id": 74,
        "account_name": "fixture-recovered",
        "model_id": "example/model",
        "recovered_at": stamp(AT + timedelta(minutes=10)),
        "reset_credit": {
            "consumed": True,
            "completed_at": stamp(AT + timedelta(minutes=9)),
            "verification_method": "model",
        },
    }
    bark_credit_event = {
        "outcome": "unknown",
        "account_id": 75,
        "account_name": "fixture-uncertain",
        "trigger_at": stamp(AT),
        "attempt_at": stamp(AT + timedelta(minutes=1)),
        "verification_method": "connection",
        "reason": "receipt unavailable",
        "completed_at": stamp(AT + timedelta(minutes=2)),
    }

    def capture_credit_message(event: dict[str, Any]) -> tuple[str, str]:
        captured: dict[str, Any] = {}

        def push(title: str, body: str, _config: Any, *, timeout: float, options: Any) -> Any:
            captured["message"] = (title, body)
            captured["timeout"] = timeout
            captured["options"] = options
            return py_bark.BarkPushResult(True)

        notifier = SimpleNamespace(_push_with_config=push)
        config = py_bark.BarkRuntimeConfig(True, True, "", "https://push.example.invalid")
        py_bark.BarkNotifier.notify_credit_result(notifier, event, config=config)
        return captured["message"]

    bark_urls = []
    for name, value in [
        ("https_service", "https://push.example.invalid/base/"),
        ("loopback_http", "http://127.0.0.1:8787/notify/"),
        ("external_http_rejected", "http://push.example.invalid/"),
        ("query_rejected", "https://push.example.invalid/?fixture=1"),
    ]:
        try:
            normalized = py_bark.normalize_bark_server_url(value)
            bark_urls.append({"name": name, "value": value, "expected": normalized, "push_url": py_bark.bark_push_url(value)})
        except ValueError:
            bark_urls.append({"name": name, "value": value, "expected_error": True})
    chunk_text = "额度恢复🙂状态更新" * 9
    bark_vectors = {
        "urls": bark_urls,
        "oauth_messages": [
            {"name": "failure_format", "event": bark_error_event,
             "expected": py_bark.oauth_event_message(bark_error_event)},
            {"name": "recovered_with_receipt", "event": bark_recovered_event,
             "expected": py_bark.oauth_event_message(bark_recovered_event)},
        ],
        "credit_message": {"event": bark_credit_event, "expected": capture_credit_message(bark_credit_event)},
        "chunks": {
            "text": chunk_text,
            "max_bytes": 48,
            "split_expected": py_bark.split_utf8_chunks(chunk_text, 48),
            "body_expected": py_bark.bark_body_parts(chunk_text, 48),
            "empty_expected": py_bark.bark_body_parts("", 48),
        },
    }

    grok_cases = [
        ("available", {"status": "active", "schedulable": True, "extra": {"grok_needs_reauth": False}}),
        ("reauth_needed", {"status": "active", "schedulable": True, "extra": {"grok_needs_reauth": True}}),
        ("malformed_status", {"status": "active", "schedulable": True, "extra": "invalid fixture"}),
        ("future_rate_reset", {"status": "active", "schedulable": True,
                                "rate_limit_reset_at": stamp(AT + timedelta(seconds=1))}),
    ]
    key_fallback_vectors = {
        "grok": [
            {"name": name, "row": row, "expected": classify_grok_oauth_account(row, AT)}
            for name, row in grok_cases
        ],
        "oauth": [],
        "desired": [],
    }
    for name, row, result in [
        ("available", account(), quota_result(AT, reset_5h=AT + timedelta(hours=1), reset_7d=AT + timedelta(days=3))),
        ("quota_exhausted", account(), quota_result(AT, used_5h=100, reset_5h=AT + timedelta(hours=1), reset_7d=AT + timedelta(days=3))),
        ("incomplete_windows", account(), {"success": True, "queried_at": stamp(AT),
             "oauth_quota": {"plan_type": "plus", "ui_windows": [{"key": "codex_5h", "used_percent": 20}]}}),
        ("stale_result", account(), quota_result(AT - timedelta(hours=2))),
    ]:
        key_fallback_vectors["oauth"].append({
            "name": name,
            "row": row,
            "result": result,
            "now": stamp(AT),
            "freshness_seconds": 3600,
            "expected": classify_oauth_account(row, result, AT, 3600),
        })
    for name, states in [
        ("empty", []),
        ("available_wins", [UNKNOWN, UNAVAILABLE, AVAILABLE]),
        ("all_unavailable", [UNAVAILABLE, UNAVAILABLE]),
        ("unknown_only", [UNKNOWN]),
    ]:
        key_fallback_vectors["desired"].append({
            "name": name,
            "states": states,
            "expected": desired_key_schedulable(states),
        })

    manual_row = account()
    with tempfile.TemporaryDirectory(prefix="server-recovery-python-") as directory:
        baseline_state = OAuthStateStore(str(Path(directory) / "oauth-state.json"))
        generation = baseline_state.manual_control(manual_row, False, AT)
        baseline_state.commit(
            results={73: {"template_type": "oauth", "stale_result": True}},
            scheduler_updates={73: {"stale_scheduler_marker": True}},
            expected_generations={73: generation - 1},
        )
        generation_snapshot = baseline_state.snapshot()
    manual_generation = {
        "row": manual_row,
        "at": stamp(AT),
        "expected_generation": generation,
        "expected_control": generation_snapshot["scheduler"]["73"]["manual_control"],
        "expected_result": generation_snapshot["oauth_results"].get("73"),
        "expected_stale_marker": generation_snapshot["scheduler"]["73"].get("stale_scheduler_marker"),
    }

    return output_value({
        "reference_sha": REFERENCE_SHA,
        "at": stamp(AT),
        "oauth_admission": {
            "fingerprint": {"row": admission_row, "expected": credential_fingerprint(admission_row)},
            "fresh_quota": fresh_quota_vectors,
            "gates": gate_vectors,
            "query_metadata": {
                "metadata": {"last_attempt_at": stamp(AT - timedelta(minutes=10)),
                             "quota_query": {"automatic_attempts": []}},
                "saved": quota_result(AT - timedelta(minutes=5)),
                "expected": query_metadata(
                    {"last_attempt_at": stamp(AT - timedelta(minutes=10)),
                     "quota_query": {"automatic_attempts": []}},
                    quota_result(AT - timedelta(minutes=5)),
                ),
            },
            "constants": {
                "min_query_seconds": MIN_QUERY_SECONDS,
                "automatic_window_seconds": AUTO_WINDOW_SECONDS,
                "automatic_query_limit": AUTO_QUERY_LIMIT,
            },
        },
        "recovery_policy": {
            "account_ids_input": [7, 2, 7, 10],
            "account_ids_expected": account_ids([7, 2, 7, 10]),
            "eligible_rows": selection_rows,
            "eligible_expected": eligible_ids(selection_rows),
            "methods": policy_methods,
        },
        "recovery_rules": {
            **rule_vectors,
            "quota_readiness": readiness_vectors,
            "manual_control": control_vectors,
            "candidates": {
                "rows": candidate_rows,
                "results": {str(key): value for key, value in candidate_results.items()},
                "scheduler": candidate_scheduler,
                "now": stamp(AT),
                "expected": candidate_projection,
            },
        },
        "daily_test": daily_vectors,
        "reset_credit": reset_vectors,
        "bark": bark_vectors,
        "key_fallback": key_fallback_vectors,
        "manual_generation": manual_generation,
    })


def main() -> None:
    repo = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="server-recovery-pinned-") as directory:
        app_root = Path(directory)
        export_pinned_app(app_root, repo)
        fixtures = build_fixtures(repo, app_root)
    json.dump(fixtures, sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
