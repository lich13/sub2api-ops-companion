#!/usr/bin/env python3
"""Build parity vectors from the immutable Python service at the migration SHA."""

from __future__ import annotations

import copy
import io
import json
import math
import subprocess
import sys
import tarfile
import tempfile
import contextlib
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
OUTPUT = ROOT / "tests" / "contracts" / "projection" / "fixtures.json"
QUOTA_AT = datetime(2026, 9, 24, 12, tzinfo=timezone.utc)
USAGE_AT = datetime(2026, 9, 25, 16, 5, tzinfo=timezone.utc)
SLOW_AT = datetime(2026, 10, 1, 12, tzinfo=timezone.utc)
REASONING_MODEL = "future-model"


def archive_reference(destination: Path) -> Path:
    result = subprocess.run(
        ["git", "archive", "--format=tar", REFERENCE_SHA, "app"],
        cwd=ROOT,
        capture_output=True,
        check=True,
    )
    with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:") as archive:
        for member in archive:
            member_path = Path(member.name)
            if member_path.is_absolute() or ".." in member_path.parts:
                raise RuntimeError("pinned source archive contains an unsafe path")
            archive.extract(member, path=destination, filter="data")
    app_dir = destination / "app"
    if not (app_dir / "account_quality.py").is_file():
        raise RuntimeError("pinned Python app could not be extracted")
    return destination


def jsonable(value: Any) -> Any:
    if isinstance(value, datetime):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(key): jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(item) for item in value]
    if isinstance(value, float) and not math.isfinite(value):
        return None
    return value


def old_account(**extra: Any) -> dict[str, Any]:
    return {
        "id": 1,
        "platform": "openai",
        "type": "oauth",
        "status": "active",
        "schedulable": True,
        "credentials": {"plan_type": "plus"},
        "extra": {
            "codex_usage_updated_at": QUOTA_AT.isoformat(),
            "codex_5h_used_percent": 0,
            "codex_7d_used_percent": 20,
            "codex_5h_reset_at": (QUOTA_AT + timedelta(hours=2)).isoformat(),
            "codex_7d_reset_at": (QUOTA_AT + timedelta(days=2)).isoformat(),
            **copy.deepcopy(extra),
        },
    }


def build_quota(reference: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference))
    from app.key_fallback import classify_oauth_account, latest_completed_oauth_result
    from app.oauth_monitor import build_monitor_candidates
    from app.quota_snapshot import FRESHNESS_SECONDS, latest_openai_result, usage_windows

    window_specs = [
        ("zero_and_unknown", old_account(codex_7d_used_percent=None), None),
        ("free_plan_only_seven_day", {**old_account(secret="fixture-private-marker"), "quota_plan_type": "free"}, None),
        (
            "new_error_precedes_old_passive_snapshot",
            old_account(codex_usage_updated_at=(QUOTA_AT - timedelta(minutes=3)).isoformat()),
            None,
        ),
        ("new_passive_replaces_old_result", old_account(), {"success": False, "queried_at": (QUOTA_AT - timedelta(seconds=1)).isoformat()}),
        ("expired_five_hour", old_account(codex_5h_reset_at=QUOTA_AT.isoformat()), None),
        (
            "grok_headers_only",
            {
                "platform": "grok",
                "type": "oauth",
                "extra": {
                    "grok_usage_snapshot": {
                        "requests": {"limit": 100, "remaining": 56},
                        "tokens": {"limit": 100000, "remaining": 75000},
                        "headers": {"authorization": "fixture-private-marker"},
                        "updated_at": QUOTA_AT.isoformat(),
                        "status_code": 200,
                    }
                },
            },
            None,
        ),
        (
            "grok_missing_snapshot",
            {"platform": "grok", "type": "oauth"},
            None,
        ),
        (
            "grok_incomplete",
            {
                "platform": "grok",
                "type": "apikey",
                "extra": {"grok_usage_snapshot": {"requests": {"limit": 100, "remaining": 0}, "tokens": {"limit": 100}, "updated_at": QUOTA_AT.isoformat(), "status_code": 200}},
            },
            None,
        ),
        (
            "grok_stale",
            {
                "platform": "grok",
                "type": "oauth",
                "extra": {"grok_usage_snapshot": {"requests": {"limit": 100, "remaining": 0}, "updated_at": QUOTA_AT.isoformat(), "status_code": 200}},
            },
            None,
        ),
        (
            "grok_error",
            {
                "platform": "grok",
                "type": "oauth",
                "extra": {"grok_usage_snapshot": {"requests": {"limit": 100, "remaining": 0}, "updated_at": QUOTA_AT.isoformat(), "status_code": 522}},
            },
            None,
        ),
        (
            "billing_and_money_are_not_usage_windows",
            {"platform": "openai", "type": "apikey", "extra": {"quota_daily_limit": 100, "quota_daily_used": 25, "grok_billing_snapshot": {"usage_percent": 10}}},
            None,
        ),
        (
            "grok_billing_not_header_quota",
            {"platform": "grok", "type": "apikey", "extra": {"quota_daily_limit": 100, "quota_daily_used": 25, "grok_billing_snapshot": {"usage_percent": 10}}},
            None,
        ),
    ]
    window_cases = []
    for name, row, result in window_specs:
        current = QUOTA_AT + timedelta(hours=2) if name == "grok_stale" else QUOTA_AT
        window_cases.append(
            {
                "id": name,
                "row": row,
                "result": result,
                "at": current.isoformat(),
                "expected": jsonable(usage_windows(row, current, result)),
            }
        )

    stale_fields = [
        {"codex_usage_updated_at": (QUOTA_AT - timedelta(seconds=3601)).isoformat()},
        {"codex_7d_used_percent": None},
        {"codex_usage_updated_at": (QUOTA_AT + timedelta(seconds=1)).isoformat()},
    ]
    classification_cases = []
    for index, changes in enumerate(stale_fields):
        row = old_account(**changes)
        selected = latest_openai_result(row, None, QUOTA_AT)
        classification_cases.append(
            {
                "id": f"incomplete_or_stale_{index}",
                "row": row,
                "result": selected,
                "freshness_seconds": 3600,
                "expected_state": classify_oauth_account(row, selected, QUOTA_AT, 3600),
            }
        )
    row = old_account(codex_7d_used_percent=None)
    selected = latest_openai_result(row, None, QUOTA_AT)
    classification_cases.append(
        {
            "id": "unknown_is_not_available",
            "row": row,
            "result": selected,
            "freshness_seconds": FRESHNESS_SECONDS,
            "expected_state": classify_oauth_account(row, selected, QUOTA_AT, FRESHNESS_SECONDS),
        }
    )
    row = old_account(codex_7d_used_percent=100, codex_7d_reset_at=QUOTA_AT.isoformat())
    selected = latest_openai_result(row, None, QUOTA_AT)
    classification_cases.append(
        {
            "id": "fresh_passive_replaces_old_active_result",
            "row": row,
            "result": selected,
            "freshness_seconds": FRESHNESS_SECONDS,
            "expected_state": classify_oauth_account(row, selected, QUOTA_AT, FRESHNESS_SECONDS),
        }
    )
    error_row = old_account(codex_usage_updated_at=(QUOTA_AT - timedelta(minutes=3)).isoformat())
    old_passive = latest_openai_result(error_row, None, QUOTA_AT)
    latest_error = latest_completed_oauth_result(
        old_passive,
        {"last_error_at": QUOTA_AT.isoformat(), "last_error_code": "http_500"},
    )
    classification_cases.append(
        {
            "id": "newer_error_overrides_passive_success",
            "row": error_row,
            "result": latest_openai_result(error_row, latest_error, QUOTA_AT),
            "freshness_seconds": FRESHNESS_SECONDS,
            "expected_state": classify_oauth_account(
                error_row,
                latest_openai_result(error_row, latest_error, QUOTA_AT),
                QUOTA_AT,
                FRESHNESS_SECONDS,
            ),
            "error_result": latest_error,
        }
    )

    depleted = old_account(codex_7d_used_percent=100, codex_7d_reset_at=QUOTA_AT.isoformat())
    no_due = old_account()
    candidate_cases = []
    for name, rows, now in [
        ("before_reset", [depleted], QUOTA_AT - timedelta(seconds=1)),
        ("at_reset", [depleted], QUOTA_AT),
        ("not_depleted", [no_due], QUOTA_AT),
    ]:
        actual = build_monitor_candidates(rows, {}, {}, now)
        candidate_cases.append(
            {
                "id": name,
                "rows": rows,
                "results": {},
                "scheduler": {},
                "at": now.isoformat(),
                "enabled": True,
                "expected": [{"account_id": v["account_id"], "reason": v["reason"]} for v in actual],
            }
        )
    disabled = build_monitor_candidates([depleted], {}, {}, QUOTA_AT, recovery_monitor_enabled=False)
    candidate_cases.append(
        {
            "id": "disabled",
            "rows": [depleted],
            "results": {},
            "scheduler": {},
            "at": QUOTA_AT.isoformat(),
            "enabled": False,
            "expected": [{"account_id": v["account_id"], "reason": v["reason"]} for v in disabled],
        }
    )

    return {
        "at": QUOTA_AT.isoformat(),
        "windows": window_cases,
        "classification": classification_cases,
        "candidate_cases": candidate_cases,
    }


def old_usage_row(platform: str = "openai", kind: str = "oauth", **extra: Any) -> dict[str, Any]:
    return {
        "id": 1,
        "name": "usage fixture",
        "platform": platform,
        "type": kind,
        "status": "active",
        "schedulable": True,
        "updated_at": USAGE_AT,
        "quota_plan_type": "plus",
        "extra": copy.deepcopy(extra),
    }


def build_usage(reference: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference))
    from app.desktop_usage import attach_stats, project_usage, read_stats, reset_credits, stats_specs

    def entries(stats: dict[tuple[int, str], dict[str, Any]]) -> list[dict[str, Any]]:
        return [
            {"account_id": account_id, "key": key, "stats": copy.deepcopy(value)}
            for (account_id, key), value in sorted(stats.items())
        ]

    credit_extra = {
        "codex_reset_credit_snapshot": {
            "available_count": 2,
            "credits": [
                {"expires_at": (USAGE_AT - timedelta(seconds=1)).isoformat()},
                {"expires_at": (USAGE_AT + timedelta(days=1)).isoformat(), "id": "fixture-expiring-credit"},
            ],
        },
        "codex_credits_snapshot": {"balance": 123},
        "referral": "fixture-referral-marker",
    }
    cases = [
        (
            "openai_window_boundary_stats_and_estimate",
            old_usage_row(
                codex_5h_used_percent=25,
                codex_7d_used_percent=99,
                codex_5h_reset_at=(USAGE_AT + timedelta(hours=2)).isoformat(),
                codex_usage_updated_at=USAGE_AT.isoformat(),
            ),
            {(1, "codex_7d"): {"cost": 76.8}},
        ),
        (
            "grok_paid_bill_not_request_headers",
            old_usage_row(
                "grok",
                subscription_tier="supergrok",
                grok_usage_snapshot={"requests": {"limit": 10, "remaining": 1}},
                grok_billing_snapshot={
                    "period_type": "weekly",
                    "usage_percent": 100,
                    "used_cents": 50,
                    "monthly_limit_cents": 100,
                    "prepaid_balance": 1.5,
                    "period_start": (USAGE_AT - timedelta(days=2)).isoformat(),
                    "period_end": (USAGE_AT + timedelta(days=5)).isoformat(),
                    "fetched_at": USAGE_AT.isoformat(),
                    "partial": True,
                    "failed_windows": ["monthly"],
                    "monthly_status_code": 502,
                },
            ),
            {},
        ),
        ("grok_free_rolling_tokens", old_usage_row("grok", subscription_tier="free"), {}),
        (
            "grok_credentials_tier_overrides_snapshot",
            old_usage_row("grok", subscription_tier="free", grok_billing_snapshot={"plan": "free"}),
            {},
        ),
        ("reset_credit_expiry_and_privacy", {**old_usage_row(**credit_extra), "parent_account_id": 2}, {}),
    ]
    for platform in ("openai", "grok"):
        cases.extend(
            [
                (
                    f"apikey_configured_quotas_{platform}",
                    old_usage_row(
                        platform,
                        "apikey",
                        quota_daily_limit=10,
                        quota_daily_used=2.5,
                        quota_weekly_limit=20,
                        quota_weekly_used=18,
                        quota_limit=100,
                        quota_used=90,
                        quota_daily_start=USAGE_AT.isoformat(),
                        quota_weekly_reset_mode="fixed",
                        quota_weekly_reset_at=(USAGE_AT + timedelta(days=1)).isoformat(),
                    ),
                    {},
                ),
                (f"apikey_unconfigured_quotas_{platform}", old_usage_row(platform, "apikey"), {}),
            ]
        )
    projections = []
    for name, row, stats in cases:
        projected = project_usage(copy.deepcopy(row), USAGE_AT)
        if name == "grok_credentials_tier_overrides_snapshot":
            paid_row = copy.deepcopy(row)
            paid_row["quota_grok_tier"] = "supergrok"
            paid = project_usage(copy.deepcopy(paid_row), USAGE_AT)
            free_row = copy.deepcopy(row)
            free_row["quota_grok_tier"] = "free"
            free_row["extra"]["grok_billing_snapshot"] = {"usage_percent": 40}
            free = project_usage(copy.deepcopy(free_row), USAGE_AT)
            projections.append(
                {
                    "id": name,
                    "row": row,
                    "stats": {},
                    "expected": jsonable(projected),
                    "tier_override_rows": {"paid": paid_row, "free": free_row},
                    "tier_override_expected": {"paid": jsonable(paid), "free": jsonable(free)},
                }
            )
            continue
        projection_expected = jsonable(projected)
        specs = stats_specs(1, projected)
        stats_input = copy.deepcopy(stats)
        if name == "grok_free_rolling_tokens":
            stats_input = {"missing": {}, "half": {"grok_24h": {"tokens": 250000}}, "zero": {"grok_24h": {"tokens": 0}}}
            stats_entries = {
                case_name: entries({(1, key): value for key, value in by_key.items()})
                for case_name, by_key in stats_input.items()
            }
        else:
            stats_entries = {"default": entries(stats)}
        attached: dict[str, Any] = {}
        if name == "grok_free_rolling_tokens":
            for case_name, by_key in stats_input.items():
                value = copy.deepcopy(projected)
                attach_stats(1, value, {(1, key): item for key, item in by_key.items()}, free_token_limit=500_000)
                attached[case_name] = jsonable(value)
        else:
            attach_stats(1, projected, stats, free_token_limit=500_000)
            attached["default"] = jsonable(projected)
        projections.append(
            {
                "id": name,
                "row": row,
                "stats": jsonable(stats_input),
                "expected": projection_expected,
                "stats_specs": jsonable(specs),
                "stats_entries": jsonable(stats_entries),
                "attached_expected": attached,
                "reset_credits_expected": jsonable(reset_credits(row.get("extra") or {}, USAGE_AT)),
            }
        )

    raw_stats_rows = [
        {"account_id": 1, "key": "today", "requests": 3, "tokens": 12, "cost": 2.0, "standard_cost": 3.0, "user_cost": 4.0},
        {"account_id": 2, "key": "codex_7d", "requests": 5, "tokens": 21, "cost": 6.5, "standard_cost": 7.5, "user_cost": 8.5},
    ]

    class StatsDb:
        def fetch_all(self, _sql: str, _params: Any) -> list[dict[str, Any]]:
            return copy.deepcopy(raw_stats_rows)

    old_stats = read_stats(StatsDb(), [{"account_id": 1, "key": "today"}, {"account_id": 2, "key": "codex_7d"}])
    stats_rows_expected = [
        {"account_id": account_id, "key": key, **copy.deepcopy(value)}
        for (account_id, key), value in sorted(old_stats.items())
    ]
    return {
        "at": USAGE_AT.isoformat(),
        "cases": projections,
        "stats_rows_input": raw_stats_rows,
        "stats_rows_expected": stats_rows_expected,
    }


def build_slow(reference: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference))
    from app.account_quality import slow_ttft_sample, slow_ttft_warning
    from app.error_evidence import ERROR_WHERE, is_local_throttle, match_message

    def sample(i: int, first: float, *, at: datetime | None = None, account_id: int = 1, **changes: Any) -> dict[str, Any]:
        row = {
            "id": i,
            "account_id": account_id,
            "created_at": (at or SLOW_AT - timedelta(minutes=i)).isoformat(),
            "model": "gpt-6-luna",
            "upstream_model": "gpt-6-luna",
            "stream": True,
            "first_token_ms": first,
            "duration_ms": max(first + 1000, 12000),
            "output_tokens": 64,
            "inbound_endpoint": "/v1/responses",
            "image_count": 0,
            "image_output_tokens": 0,
            "video_count": 0,
            "video_duration_seconds": 0,
        }
        row.update(changes)
        return row

    def slow_rows(firsts: list[float], *, account: dict[str, Any] | None = None) -> list[Any]:
        account = account or {"id": 1, "platform": "openai", "type": "oauth"}
        return [slow_ttft_sample(sample(i, value), account) for i, value in enumerate(firsts)]

    threshold_rows = slow_rows([11001 if i < 8 else 9000 for i in range(10)])
    fast_reset_rows = list(threshold_rows)
    fast_reset_rows[0] = slow_ttft_sample(sample(0, 10000), {"id": 1, "platform": "openai", "type": "oauth"})
    old_sample_rows = slow_rows([11001 if i < 8 else 9000 for i in range(10)], account={"id": 1, "platform": "openai", "type": "apikey"})
    old_sample_rows[9] = slow_ttft_sample(
        sample(9, 11001, at=SLOW_AT - timedelta(days=2)),
        {"id": 1, "platform": "openai", "type": "apikey"},
    )

    warning_cases = [
        {"id": "eight_slow_and_latest_slow", "inputs": [sample(i, 11001 if i < 8 else 9000) for i in range(10)], "account": {"id": 1, "platform": "openai", "type": "oauth"}},
        {"id": "latest_fast_resets_active", "inputs": [sample(0, 10000), *[sample(i, 11001 if i < 8 else 9000) for i in range(1, 10)]], "account": {"id": 1, "platform": "openai", "type": "oauth"}},
        {"id": "nine_samples_not_enough", "inputs": [sample(i, 11001 if i < 8 else 9000) for i in range(9)], "account": {"id": 1, "platform": "openai", "type": "apikey"}},
        {"id": "old_latest_outside_day", "inputs": [sample(i, 11001 if i < 8 else 9000) for i in range(9)] + [sample(9, 11001, at=SLOW_AT - timedelta(days=2))], "account": {"id": 1, "platform": "openai", "type": "apikey"}},
    ]
    for case in warning_cases:
        projected = [slow_ttft_sample(row, case["account"]) for row in case["inputs"]]
        case["expected_samples"] = [
            None
            if value is None
            else {"id": value["id"], "at": value["at"].isoformat(), "first_token_ms": value["first_token_ms"]}
            for value in projected
        ]
        case["expected_warning"] = jsonable(slow_ttft_warning([value for value in projected if value], SLOW_AT))

    exclusions = [
        {"id": "non_stream", "row": sample(1, 11001, stream=False)},
        {"id": "image", "row": sample(2, 11001, image_count=1)},
        {"id": "duration_not_after_ttft", "row": sample(3, 11001, duration_ms=10000)},
        {"id": "grok_platform", "row": sample(4, 11001), "account": {"id": 1, "platform": "grok", "type": "oauth"}},
    ]
    for case in exclusions:
        account = case.get("account", {"id": 1, "platform": "openai", "type": "oauth"})
        value = slow_ttft_sample(case["row"], account)
        case["expected"] = None if value is None else {"id": value["id"], "at": value["at"].isoformat(), "first_token_ms": value["first_token_ms"]}

    throttle_text = "您已达到请求数限制：1分钟内最多请求 15 次 (request id: fixture-req-1)"
    throttle_cases = []
    for field in ("message", "error_message", "upstream_error_message"):
        for nested in (False, True):
            row = {field: json.dumps({"request": {"error": {"message": throttle_text}}}, ensure_ascii=False) if nested else throttle_text}
            throttle_cases.append({"id": f"{field}_{'nested' if nested else 'plain'}", "row": row, "expected": is_local_throttle(row)})
    for index, text in enumerate(
        (
            "您已达到请求数限制：每分钟最多请求 15 次",
            "您 已 超过 请求数量 限制 - 每 1 分钟 至多 请求 15 次",
            "You have reached the rate limit: 15 requests per minute",
            "You've exceeded the request rate limit: maximum 15 requests / minute",
            "maximum number of requests: 15 requests per minute",
        )
    ):
        row = {"message": text}
        throttle_cases.append({"id": f"plain_variant_{index}", "row": row, "expected": is_local_throttle(row)})
    throttle_cases.extend(
        [
            {"id": "error_body_only", "row": {"error_body": json.dumps({"error": {"message": throttle_text}}, ensure_ascii=False)}, "expected": is_local_throttle({"error_body": json.dumps({"error": {"message": throttle_text}}, ensure_ascii=False)})},
            {"id": "upstream_errors_only", "row": {"upstream_errors": [{"message": throttle_text}]}, "expected": is_local_throttle({"upstream_errors": [{"message": throttle_text}]})},
            {"id": "provider_rate_is_not_local", "row": {"message": "Rate limit exceeded", "upstream_status_code": 429}, "expected": is_local_throttle({"message": "Rate limit exceeded", "upstream_status_code": 429})},
        ]
    )

    capacity_message = "Our servers are currently overloaded. Please try again later."
    local_message = "您已达到请求数限制：每分钟最多请求 15 次"
    evidence_rows = [
        {"account_platform": "openai", "account_type": "oauth", "error_owner": "provider", "error_phase": "upstream", "upstream_error_message": capacity_message},
        {"account_platform": "openai", "account_type": "oauth", "error_owner": "provider", "error_phase": "upstream", "upstream_errors": [{"message": capacity_message}]},
        {"account_platform": "openai", "account_type": "oauth", "error_owner": "provider", "error_phase": "upstream", "upstream_errors": [{"message": local_message}, {"message": "Selected model is at capacity. Please try a different model."}]},
    ]
    evidence_cases = []
    for index, row in enumerate(evidence_rows):
        evidence_cases.append(
            {
                "id": f"capacity_vs_local_{index}",
                "row": row,
                "expected_message": match_message(row),
                "is_local": is_local_throttle(row),
            }
        )
    warning = slow_ttft_warning([value for value in threshold_rows if value], SLOW_AT)
    reset_warning = slow_ttft_warning([value for value in fast_reset_rows if value], SLOW_AT)
    too_old_warning = slow_ttft_warning([value for value in old_sample_rows if value], SLOW_AT)

    return {
        "at": SLOW_AT.isoformat(),
        "warning_cases": warning_cases,
        "sample_exclusions": exclusions,
        "throttle_cases": throttle_cases,
        "capacity_cases": evidence_cases,
        "threshold_expected": jsonable(warning),
        "fast_reset_expected": jsonable(reset_warning),
        "old_latest_expected": jsonable(too_old_warning),
        "error_where_present": bool(ERROR_WHERE),
    }


def build_identity(reference: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference))
    from app.codex_identity import codex_identity, synced_codex_user_agent

    cases = [
        {
            "id": "empty_user_agent_uses_synced_version",
            "input": {"openai_codex_client_version_synced": "0.159.3"},
            "expected": codex_identity({"openai_codex_client_version_synced": "0.159.3"}),
        },
        {
            "id": "custom_leading_and_trailing_versions_rewritten",
            "input": {"custom": "codex-tui/0.146.1 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.146.1)", "version": "0.160.0"},
            "expected": synced_codex_user_agent(
                "codex-tui/0.146.1 (macOS 15.6; arm64) iTerm2 (codex-tui; 0.146.1)", "0.160.0"
            ),
        },
        {
            "id": "invalid_version_and_newline_fallback",
            "input": {
                "openai_codex_client_version": "latest",
                "openai_codex_client_version_synced": "bad",
                "openai_codex_user_agent": "codex-tui/0.1.0\nunsafe",
            },
            "expected": codex_identity(
                {
                    "openai_codex_client_version": "latest",
                    "openai_codex_client_version_synced": "bad",
                    "openai_codex_user_agent": "codex-tui/0.1.0\nunsafe",
                }
            ),
        },
    ]
    return jsonable({"cases": cases})


def reasoning_inputs() -> dict[str, Any]:
    descriptor = {
        "slug": REASONING_MODEL,
        "display_name": "Fixture future model",
        "context_window": 123456,
        "input_modalities": ["text"],
        "model_messages": {"instructions_template": "fixture", "keep": False},
        "supported_reasoning_levels": [{"effort": "low", "description": "Quick"}, {"effort": "max", "description": "Deep"}],
        "default_reasoning_level": "max",
        "unknown": {"zero": 0, "null": None},
    }
    account = {
        "id": 1,
        "platform": "openai",
        "type": "apikey",
        "status": "disabled",
        "schedulable": False,
        "credentials": {"model_mapping": {REASONING_MODEL: REASONING_MODEL}, "base_url": "https://gateway.example.invalid"},
        "extra": {"openai_responses_supported": True},
    }
    group = {
        "id": 7,
        "name": "Fixture group",
        "platform": "openai",
        "model_allowlist": {"enabled": False, "models": []},
        "codex_models_manifest_config": {},
        "updated_at": "2031-04-05",
    }
    return {"descriptor": descriptor, "account": account, "group": group}


def build_reasoning(reference: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference))
    from app.model_reasoning import complete_descriptor, forwarding, model_id, patch_descriptor, reasoning_fields, reasoning_values, routing_binding, same_reasoning
    from app.model_catalog import ModelCatalogService
    from app.model_config_store import ModelConfigStore
    from app.model_rules import revision, transform

    sample = reasoning_inputs()
    descriptor, account, group = sample["descriptor"], sample["account"], sample["group"]
    fields = reasoning_fields(["low", "high"], "high")
    descriptor_cases = [
        {
            "id": "descriptor_complete_and_exact_identity",
            "descriptor": descriptor,
            "fields": fields,
            "expected_patch": patch_descriptor(descriptor, fields),
            "expected_complete": complete_descriptor(descriptor, REASONING_MODEL),
            "expected_other_target_complete": complete_descriptor(descriptor, "different"),
        },
        {
            "id": "id_only_descriptor_is_incomplete",
            "descriptor": {"slug": REASONING_MODEL},
            "fields": fields,
            "expected_patch": patch_descriptor({"slug": REASONING_MODEL}, fields),
            "expected_complete": complete_descriptor({"slug": REASONING_MODEL}, REASONING_MODEL),
        },
    ]
    identity_cases = []
    for value in (REASONING_MODEL, "gpt-*", " leading", "trailing ", "has\nnewline"):
        try:
            expected = {"ok": model_id(value)}
        except ValueError:
            expected = {"error": True}
        identity_cases.append({"id": f"model_id_{len(identity_cases)}", "value": value, "expected": expected})
    field_inputs = [
        ("valid_sorted_efforts", ["low", "high"], "high"),
        ("empty", [], ""),
        ("default_not_selected", ["low"], "high"),
        ("duplicate", ["low", "low"], "low"),
        ("non_string", [{}], "low"),
    ]
    field_cases = []
    for name, efforts, default in field_inputs:
        try:
            expected = {"ok": reasoning_fields(efforts, default)}
        except ValueError:
            expected = {"error": True}
        field_cases.append({"id": name, "efforts": efforts, "default": default, "expected": expected})

    group_route = {
        "enabled": True,
        "id": 1,
        "public_model": "alias",
        "match_type": "exact",
        "target_platform": "openai",
        "upstream_model": REASONING_MODEL,
        "endpoint": "responses",
    }
    composite = {**group, "platform": "composite"}
    route_cases = []

    def add_route_case(name: str, group_row: dict[str, Any], model: str, accounts: list[dict[str, Any]], routes: list[dict[str, Any]], efforts: list[str], version: str) -> None:
        binding, available = routing_binding(group_row, model, accounts, routes)
        result = forwarding(group_row, model, efforts, available, version)
        route_cases.append(
            {
                "id": name,
                "group": group_row,
                "model": model,
                "accounts": accounts,
                "routes": routes,
                "efforts": efforts,
                "version": version,
                "expected_binding": binding,
                "expected_target_count": len(available),
                "expected_state": result,
            }
        )

    add_route_case("composite_openai_verified", composite, "alias", [account], [group_route], ["max"], "0.2.10")
    limited = {**composite, "reasoning_effort_mappings": [{"model": "alias", "match_type": "exact", "from": "max", "to": "high"}]}
    add_route_case("composite_policy_mapping_limited", limited, "alias", [account], [group_route], ["max"], "0.2.10")
    ceiling = {**composite, "max_reasoning_effort": "high"}
    add_route_case("composite_ceiling_limited", ceiling, "alias", [account], [group_route], ["max"], "0.2.10")
    add_route_case("unknown_version_unverified", composite, "alias", [account], [group_route], ["max"], "future")
    other_account = {**account, "id": 2, "credentials": {"model_mapping": {REASONING_MODEL: "other-target"}}}
    add_route_case("different_targets_unverified", composite, "alias", [account, other_account], [group_route], ["max"], "0.2.10")
    for model, effort in [("grok-4.7", "xhigh"), ("grok-4.7", "max"), ("grok-4.5", "xhigh"), ("grok-future", "high")]:
        grok_account = {**account, "platform": "grok", "credentials": {"model_mapping": {model: model}}}
        add_route_case(f"grok_{model}_{effort}", {**group, "platform": "grok"}, model, [grok_account], [], [effort], "0.2.10")
    before_binding, before_targets = routing_binding(group, REASONING_MODEL, [account], [])
    unsupported = {**account, "extra": {"openai_responses_supported": False}}
    unsupported_binding, unsupported_targets = routing_binding(group, REASONING_MODEL, [unsupported], [])
    forced = {**unsupported, "extra": {"openai_responses_supported": False, "openai_responses_mode": "force_responses"}}
    forced_binding, forced_targets = routing_binding(group, REASONING_MODEL, [forced], [])
    protocol_cases = [
        {
            "id": "protocol_support_change_invalidates_binding",
            "group": group,
            "model": REASONING_MODEL,
            "efforts": ["max"],
            "version": "0.2.10",
            "routes": [],
            "before_accounts": [account],
            "after_accounts": [unsupported],
            "before": before_binding,
            "after": unsupported_binding,
            "expected_state": forwarding(group, REASONING_MODEL, ["max"], unsupported_targets, "0.2.10"),
        },
        {
            "id": "force_responses_restores_verified",
            "group": group,
            "model": REASONING_MODEL,
            "efforts": ["max"],
            "version": "0.2.10",
            "routes": [],
            "before_accounts": [unsupported],
            "after_accounts": [forced],
            "before": unsupported_binding,
            "after": forced_binding,
            "expected_state": forwarding(group, REASONING_MODEL, ["max"], forced_targets, "0.2.10"),
        },
    ]
    scope_group = {
        **group,
        "reasoning_effort_mappings": [
            {"from": "max", "to": "high"},
            {"from": "max", "to": "max", "model": REASONING_MODEL.upper(), "match_type": "exact"},
        ],
    }
    scope_binding, scope_targets = routing_binding(scope_group, REASONING_MODEL, [account], [])
    scope_limited = copy.deepcopy(scope_group)
    scope_limited["reasoning_effort_mappings"][1]["to"] = "deny"
    limited_binding, limited_targets = routing_binding(scope_limited, REASONING_MODEL, [account], [])
    policy_cases = [
        {"id": "exact_case_scope_wins", "group": scope_group, "binding": scope_binding, "expected": forwarding(scope_group, REASONING_MODEL, ["max"], scope_targets, "0.2.10")},
        {"id": "exact_case_denial_wins", "group": scope_limited, "binding": limited_binding, "expected": forwarding(scope_limited, REASONING_MODEL, ["max"], limited_targets, "0.2.10")},
    ]

    # Exercise the old catalog's active projection as a pure reader. Its helper
    # does not need a database or an upstream connection.
    active_group = {**group, "model_allowlist": {"enabled": False, "models": []}}
    active_binding, _ = routing_binding(active_group, REASONING_MODEL, [account], [])
    saved = {
        "overrides": {},
        "reasoning": {
            REASONING_MODEL: {
                "efforts": ["low", "high"],
                "default_effort": "high",
                "binding": active_binding,
                "mode": "active",
                "descriptor": descriptor,
            }
        },
    }
    original = {**descriptor, "slug": "native-old"}
    live_body = {"models": [original]}
    old_catalog = ModelCatalogService.__new__(ModelCatalogService)
    old_patches, old_supplements = old_catalog.active_reasoning(
        live_body, active_group, saved, [account], [], "0.2.10"
    )
    expected_live = transform(live_body, old_patches, active_group["model_allowlist"], old_supplements)
    active_projection = {
        "body": live_body,
        "group": active_group,
        "saved": saved,
        "accounts": [account],
        "routes": [],
        "version": "0.2.10",
        "expected_overrides": old_patches,
        "expected_supplements": old_supplements,
        "expected_live": expected_live,
    }

    store_contract = {
        "group_id": 7,
        "model": REASONING_MODEL,
        "initial_overrides": {
            REASONING_MODEL: {
                **reasoning_fields(["low"], "low"),
                "description": "preserve this unrelated field",
                "future": {"flag": False},
            }
        },
        "entry": {
            "efforts": ["low", "high"],
            "default_effort": "high",
            "binding": active_binding,
            "source": "manual",
            "mode": "active",
            "descriptor": descriptor,
        },
    }
    with tempfile.TemporaryDirectory(prefix="sub2ops-reasoning-store-reference-") as directory:
        old_store = ModelConfigStore(str(Path(directory) / "model-config.json"))
        first = old_store.save(7, store_contract["initial_overrides"], revision({}))
        saved = old_store.save_reasoning(
            7, REASONING_MODEL, store_contract["entry"], first["revision"]
        )
        try:
            old_store.save_reasoning(7, REASONING_MODEL, None, first["revision"])
            stale_revision_rejected = False
        except Exception:
            stale_revision_rejected = True
        removed = old_store.save_reasoning(7, REASONING_MODEL, None, saved["revision"])
        store_contract.update(
            {
                "expected_saved_overrides": saved["overrides"],
                "expected_saved_entry": saved["reasoning"][REASONING_MODEL],
                "expected_removed_overrides": removed["overrides"],
                "expected_removed_reasoning": removed.get("reasoning", {}),
                "stale_revision_rejected": stale_revision_rejected,
            }
        )

    return jsonable(
        {
            "descriptor": descriptor,
            "account": account,
            "group": group,
            "model_id_cases": identity_cases,
            "reasoning_field_cases": field_cases,
            "descriptor_cases": descriptor_cases,
            "reasoning_value_cases": [
                {
                    "id": "mixed_string_and_object_efforts",
                    "descriptor": {**descriptor, "supported_reasoning_levels": ["low", {"effort": "max"}, {"effort": "unsupported"}], "default_reasoning_level": "max"},
                    "expected_values": reasoning_values({**descriptor, "supported_reasoning_levels": ["low", {"effort": "max"}, {"effort": "unsupported"}], "default_reasoning_level": "max"}),
                },
                {
                    "id": "invalid_default_is_cleared",
                    "descriptor": {**descriptor, "supported_reasoning_levels": [{"effort": "low"}], "default_reasoning_level": "max"},
                    "expected_values": reasoning_values({**descriptor, "supported_reasoning_levels": [{"effort": "low"}], "default_reasoning_level": "max"}),
                },
            ],
            "same_reasoning_cases": [
                {
                    "id": "same_efforts_ignore_order",
                    "left": descriptor,
                    "right": {
                        **descriptor,
                        "supported_reasoning_levels": list(reversed(descriptor["supported_reasoning_levels"])),
                    },
                    "expected": same_reasoning(
                        descriptor,
                        {
                            **descriptor,
                            "supported_reasoning_levels": list(reversed(descriptor["supported_reasoning_levels"])),
                        },
                    ),
                },
                {"id": "different_default", "left": descriptor, "right": {**descriptor, "default_reasoning_level": "low"}, "expected": same_reasoning(descriptor, {**descriptor, "default_reasoning_level": "low"})},
            ],
            "route_cases": route_cases,
            "protocol_cases": protocol_cases,
            "policy_cases": policy_cases,
            "active_projection": active_projection,
            "store_contract": store_contract,
            "service_gaps": [],
        }
    )


def build_key_support(reference: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference))
    from app.account_templates import combine
    from app.error_evidence import MESSAGES

    profile = {
        "whitelist": ["fixture-model"],
        "mappings": [
            {"source": "fixture-input-*", "target": "fixture-model"},
            {"source": "fixture-terra-*", "target": "fixture-model"},
        ],
    }
    return {
        "profile": profile,
        "expected_mapping": combine(profile),
        "capacity_messages": list(MESSAGES),
        "account_before": {
            "id": 501,
            "name": "fixture-key-account",
            "platform": "openai",
            "type": "apikey",
            "deleted_at": None,
            "parent_account_id": None,
            "passthrough": False,
            "model_mapping": {},
            "proxy_id": 9,
            "group_ids": [13],
            "extra": {"pool": {"size": 4}},
            "status": "disabled",
            "schedulable": False,
        },
        "service_gap": "The old test verifies a database-backed apply and readback through a writer; this fixture only pins profile composition and the Rust suite does not perform that side effect.",
    }


def legacy_test_ids() -> list[str]:
    document = json.loads((ROOT / "tests" / "contracts" / "migration-map.json").read_text(encoding="utf-8"))
    counts = {
        "tests.test_account_quality": 28,
        "tests.test_quota_snapshot": 9,
        "tests.test_desktop_usage": 14,
        "tests.test_slow_ttft026": 7,
        "tests.test_codex_identity": 3,
        "tests.test_model_reasoning": 12,
        "tests.test_key_support023": 2,
    }
    selected = sorted(
        item["id"]
        for item in document["tests"]
        if item.get("source", {}).get("module") in counts
    )
    observed = {module: 0 for module in counts}
    for item in document["tests"]:
        module = item.get("source", {}).get("module")
        if module in observed:
            observed[module] += 1
    if observed != counts or len(selected) != sum(counts.values()):
        raise RuntimeError(f"pinned legacy test inventory changed: {observed}")
    return selected


def main() -> int:
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sub2ops-projection-reference-") as directory:
        reference = archive_reference(Path(directory))
        fixture = jsonable({
            "schema_version": 1,
            "reference_sha": REFERENCE_SHA,
            "synthetic_only": True,
            "legacy_test_ids": legacy_test_ids(),
            "quota": build_quota(reference),
            "usage": build_usage(reference),
            "slow": build_slow(reference),
            "identity": build_identity(reference),
            "reasoning": build_reasoning(reference),
            "key_support": build_key_support(reference),
        })
    OUTPUT.write_text(json.dumps(fixture, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(f"Wrote {OUTPUT.relative_to(ROOT)} from {REFERENCE_SHA}; legacy tests: {len(fixture['legacy_test_ids'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
