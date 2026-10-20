#!/usr/bin/env python3
"""Generate pinned account-mutation oracles and legacy test migration evidence."""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
CONTRACT_DIR = ROOT / "tests" / "contracts" / "account-mutation"
FIXTURE_PATH = CONTRACT_DIR / "fixtures.json"
MIGRATION_PATH = CONTRACT_DIR / "migration-fragment.json"
LEGACY_MODULES = {
    "test_account_batch031": 9,
    "test_account_operations023": 7,
    "test_account_settings035": 10,
    "test_account_templates027": 13,
    "test_group_management": 7,
    "test_native_batch038": 21,
    "test_proxy_batch036": 11,
    "test_proxy_diagnostics037": 6,
    "test_proxy_native_http037": 2,
    "test_desktop_actions": 10,
}


def check_reference(repo: Path) -> None:
    subprocess.run(
        ["git", "cat-file", "-e", f"{REFERENCE_SHA}^{{commit}}"],
        cwd=repo,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )


def git_show(repo: Path, path: str) -> bytes:
    return subprocess.run(
        ["git", "show", f"{REFERENCE_SHA}:{path}"],
        cwd=repo,
        check=True,
        capture_output=True,
    ).stdout


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def export_pinned_app(destination: Path, repo: Path) -> None:
    # Read every file from the immutable reference tree. The current checkout may
    # advance after the 0.1.40 migration without changing this oracle.
    listed = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", REFERENCE_SHA, "--", "app"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    paths = [line for line in listed.stdout.splitlines() if line.startswith("app/")]
    if not paths:
        raise RuntimeError(f"pinned source {REFERENCE_SHA} has no app tree")
    for relative in paths:
        output = git_show(repo, relative)
        path = destination / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(output)


def parse_assignment(source: str, class_name: str, method_name: str, variable: str) -> Any:
    module = ast.parse(source)
    for class_node in module.body:
        if not isinstance(class_node, ast.ClassDef) or class_node.name != class_name:
            continue
        for method in class_node.body:
            if not isinstance(method, (ast.FunctionDef, ast.AsyncFunctionDef)) or method.name != method_name:
                continue
            for child in ast.walk(method):
                if not isinstance(child, ast.Assign):
                    continue
                names = [target.id for target in child.targets if isinstance(target, ast.Name)]
                if variable in names:
                    return ast.literal_eval(child.value)
    raise RuntimeError(f"pinned test has no literal {variable}: {class_name}.{method_name}")


def python_oracles(repo: Path) -> dict[str, Any]:
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(ROOT / "scripts"))
    try:
        from server_foundation_fixtures import export_pinned_app as export_foundation_app
    except ImportError as exc:
        raise RuntimeError("cannot import the existing fixed-SHA fixture exporter") from exc

    with tempfile.TemporaryDirectory(prefix="sub2ops-pinned-account-mutation-") as name:
        scratch = Path(name)
        app_root = scratch / "source"
        export_foundation_app(app_root, repo)
        sys.path.insert(0, str(app_root))

        from app import account_templates as template_module
        from app.account_templates import AccountTemplates, combine, split
        from app.desktop_actions import (
            billing_is_fresh,
            exact_model_options,
            filter_model_options,
            is_text_model_id,
        )
        from app.proxy_diagnostics import safe_result

        template_root = scratch / "templates"
        template_root.mkdir(mode=0o700)
        full_mapping = {
            "fixture-full-model": "fixture-full-model",
            "fixture-full-alt": "fixture-full-alt",
            "fixture-full-third": "fixture-full-third",
        }
        degraded_mapping = {
            "fixture-model": "fixture-model",
            "fixture-input-*": "fixture-model",
            "fixture-terra-*": "fixture-model",
        }
        rows = {
            1: {
                "id": 1,
                "name": "fixture-full",
                "platform": "openai",
                "type": "oauth",
                "deleted_at": None,
                "parent_account_id": None,
                "passthrough": False,
                "model_mapping": full_mapping,
            },
            2: {
                "id": 2,
                "name": "fixture-degraded",
                "platform": "openai",
                "type": "apikey",
                "deleted_at": None,
                "parent_account_id": None,
                "passthrough": False,
                "model_mapping": degraded_mapping,
            },
        }

        class FixtureDB:
            def fetch_one(self, _sql: str, params: dict[str, Any]) -> dict[str, Any] | None:
                return copy.deepcopy(rows.get(params["id"]))

            def fetch_all(self, _sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
                ids = (params or {}).get("ids", list(rows))
                return [copy.deepcopy(rows[item]) for item in ids if item in rows]

        settings = type(
            "SettingsFixture",
            (),
            {
                "usage_query_state_path": str(template_root / "oauth-state.json"),
                "audit_path": str(template_root / "audit.jsonl"),
            },
        )()
        service = type("ServiceFixture", (), {"r": type("RuntimeFixture", (), {"db": FixtureDB(), "settings": settings})()})()
        templates = AccountTemplates(service)
        default_view = templates.initialize_sources()
        initialized_view = templates.initialize_sources(1, 2)
        repeated_view = templates.initialize_sources(1, 2)

        profile = {
            "whitelist": ["fixture-model"],
            "mappings": [{"source": "fixture-input-*", "target": "fixture-model"}],
        }
        combined = combine(profile)
        split_combined = split(combined)

        desktop_test_source = git_show(repo, "tests/test_desktop_actions.py").decode("utf-8")
        sse_chunks = parse_assignment(
            desktop_test_source,
            "DesktopStreamTests",
            "test_real_sse_keeps_whitespace_and_releases_lock_without_confirmation",
            "chunks",
        )
        model_items = [
            {"id": "fixture-model-a", "display_name": "fixture-model-a", "type": "text"},
            {"id": "fixture-model-b", "display_name": "fixture-model-b", "type": "text"},
            {"id": "fixture-image-model", "display_name": "fixture-image-model", "type": "image"},
        ]
        allowlists = [
            {
                "enabled": True,
                "models": [
                    "fixture-model-a",
                    "fixture-not-in-catalog",
                    "fixture-model-*",
                    "fixture-image-model",
                    "",
                ],
            },
            {"enabled": True, "models": ["fixture-model-b", "fixture-model-a"]},
        ]
        desktop = {
            "allowlists": allowlists,
            "catalog": model_items,
            "exact_candidates": exact_model_options(allowlists),
            "filtered_model_options": filter_model_options(model_items, allowlists),
            "unrestricted_text_catalog": [
                item
                for item in filter_model_options(model_items, [])
                if is_text_model_id(item["id"])
            ],
            "sse_chunks": sse_chunks,
        }
        billing_query = datetime(2026, 9, 25, 8, 0, tzinfo=timezone.utc)
        billing_cases = {
            "fresh": {
                "age_seconds": 0,
                "grok_billing": {
                    "fetched_at": billing_query.isoformat(),
                    "partial": False,
                },
            },
            "stale": {
                "age_seconds": -1,
                "grok_billing": {
                    "fetched_at": (billing_query - timedelta(seconds=1)).isoformat(),
                },
            },
            "partial": {
                "age_seconds": 0,
                "grok_billing": {
                    "fetched_at": billing_query.isoformat(),
                    "partial": True,
                },
            },
        }
        desktop_grok_billing = {
            "queried_at": billing_query.isoformat(),
            "cases": {
                name: {
                    **case,
                    "accepted": billing_is_fresh(
                        {"grok_billing": case["grok_billing"]},
                        billing_query.isoformat(),
                    ),
                }
                for name, case in billing_cases.items()
            },
        }

        proxy_test_input = {
            "success": True,
            "message": "fixture diagnostic ok",
            "latency_ms": 12,
            "ip_address": "203.0.113.7",
            "city": "Fixture City",
            "region": "Fixture Region",
            "country": "Fixture Country",
            "country_code": "ZZ",
            "address": "secret-address.example.invalid",
            "username": "fixture-private-user",
            "password": "fixture-private-password",
            "token": "fixture-private-token",
        }
        proxy_quality_input = {
            "success": True,
            "message": "fixture diagnostic ok",
            "latency_ms": 12,
            "ip_address": "203.0.113.7",
            "city": "Fixture City",
            "region": "Fixture Region",
            "country": "Fixture Country",
            "country_code": "ZZ",
            "score": 91,
            "grade": "A",
            "summary": "fixture quality",
            "exit_ip": "203.0.113.7",
            "base_latency_ms": 8,
            "passed_count": 1,
            "warn_count": 0,
            "failed_count": 0,
            "challenge_count": 0,
            "checked_at": 1735689600,
            "items": [
                {
                    "target": "https://example.invalid",
                    "status": "pass",
                    "http_status": 200,
                    "latency_ms": 12,
                    "message": "fixture pass",
                    "token": "fixture-private-token",
                }
            ],
            "address": "secret-address.example.invalid",
            "username": "fixture-private-user",
            "password": "fixture-private-password",
            "token": "fixture-private-token",
        }
        proxy = {
            "test_input": proxy_test_input,
            "test_result": safe_result("test", proxy_test_input),
            "quality_input": proxy_quality_input,
            "quality_result": safe_result("quality-check", proxy_quality_input),
        }
        native_http_test_input = {
            "success": True,
            "latency_ms": 13,
            "ip_address": "192.0.2.8",
            "message": "proxy https://fixture-user:fixture-pass@example.invalid",
        }
        native_http_quality_input = {
            "score": 80,
            "grade": "yellow",
            "checked_at": 1791774000,
            "items": [
                {
                    "target": "OpenAI",
                    "status": "challenge",
                    "http_status": 403,
                    "latency_ms": 21,
                    "message": "challenge",
                }
            ],
        }
        proxy_native_http = {
            "test_input": native_http_test_input,
            "test_result": safe_result("test", native_http_test_input),
            "quality_input": native_http_quality_input,
            "quality_result": safe_result("quality-check", native_http_quality_input),
        }

        return {
            "schema_version": 1,
            "reference": {"sha": REFERENCE_SHA, "kind": "pinned Git commit"},
            "legacy_sources": legacy_source_records(repo),
            "python_oracle": {
                "templates": {
                    "profile": profile,
                    "combined": combined,
                    "split_combined": split_combined,
                    "full_source_mapping": full_mapping,
                    "degraded_source_mapping": degraded_mapping,
                    "default_view": default_view,
                    "initialized_view": initialized_view,
                    "idempotent_repeat_version": repeated_view["version"],
                },
                "desktop_actions": desktop,
                "desktop_grok_billing": desktop_grok_billing,
                "proxy_native_http": proxy_native_http,
                "proxy_diagnostics": proxy,
            },
        }


def legacy_source_records(repo: Path) -> list[dict[str, Any]]:
    records = []
    for module, expected_count in LEGACY_MODULES.items():
        path = f"tests/{module}.py"
        source_bytes = git_show(repo, path)
        source = source_bytes.decode("utf-8")
        tree = ast.parse(source, filename=path)
        functions = [
            node
            for parent in tree.body
            if isinstance(parent, ast.ClassDef)
            for node in parent.body
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name.startswith("test_")
        ]
        if len(functions) != expected_count:
            raise RuntimeError(f"{path} has {len(functions)} tests; expected {expected_count}")
        records.append(
            {
                "path": path,
                "module": f"tests.{module}",
                "sha256": sha256_bytes(source_bytes),
                "test_count": len(functions),
            }
        )
    return records


RUST_MUTATION = "server/tests/account_mutation_parity.rs"
RUST_OPERATIONS = "server/tests/operation_contract.rs"
RUST_CONFIG = "server/tests/config_catalog_contract.rs"
RUST_FOUNDATION = "server/tests/foundation_compatibility.rs"


def target(name: str, path: str = RUST_MUTATION) -> dict[str, str]:
    return {"path": path, "test": name}


def row(
    module: str,
    class_name: str,
    method: str,
    targets: list[dict[str, str]],
    assertions: list[str],
    uncovered: tuple[str, ...] = (),
    fixtures: tuple[str, ...] = (),
) -> tuple[str, str, str, dict[str, Any]]:
    return (
        module,
        class_name,
        method,
        {
            "targets": targets,
            "covered_assertions": assertions,
            "uncovered_assertions": list(uncovered),
            "fixture_anchors": list(fixtures),
        },
    )


DB_FIXTURE = "server/tests/account_mutation_parity.rs#Harness (fresh synthetic PostgreSQL rows and real readback)"
ORACLE_TEMPLATE = "tests/contracts/account-mutation/fixtures.json#/python_oracle/templates"
ORACLE_DESKTOP = "tests/contracts/account-mutation/fixtures.json#/python_oracle/desktop_actions"
ORACLE_PROXY = "tests/contracts/account-mutation/fixtures.json#/python_oracle/proxy_diagnostics"
CONFIG_TEMPLATE = "tests/contracts/config-catalog-fixtures.json#/templates"
OPERATION_ORACLE = "tests/contracts/operation-fixtures.json#/native_bulk"


MAPPING_ROWS = [
    # Account template batches (031).
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_batch_commit_is_atomic_idempotent_and_secret_free", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["same request remains idempotent after reopening; saved batch and all four child jobs are preserved without duplicate writes","an operation-store commit failure leaves no partial batch or jobs, and a later commit failure preserves existing state","native write occurs only after dispatch; exact credentials.model_mapping payload and PostgreSQL readback are verified","admin key and synthetic account credential never appear in persisted operation state or native payload"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_busy_child_does_not_block_other_accounts_and_batch_remains_readable", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["locked account remains queued while free peer writes and completes; releasing the real account lease dispatches the locked account once; batch history remains readable"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_listing_by_batch_returns_full_terminal_history", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["batch listing contains all four children, terminal states, and pending=0 after dispatch"], (), (DB_FIXTURE,)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_queued_batch_survives_service_restart_without_duplicate_children", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["reopened Operations returns the same batch and child IDs for the same request before dispatch","the reopened queue dispatches each accepted eligible child exactly once and retains terminal history"], (), (DB_FIXTURE,)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_target_template_change_requires_confirmation_without_account_write", [target("queued_template_batch_rechecks_changed_targets_but_preserves_frozen_dispatched_target")], ["queued batch sees changed selected profile, becomes needs_confirmation, sends no native write, and leaves account mapping unchanged"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_queued_batch_target_deletion_requires_confirmation_without_account_write", [target("queued_template_batch_rechecks_changed_targets_but_preserves_frozen_dispatched_target")], ["deleting the selected queued profile before dispatch changes the child to needs_confirmation","the unchanged PostgreSQL account mapping and native bulk-write count prove no account write was sent"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_dispatched_timeout_checks_frozen_target_after_template_edit_and_deletion", [target("queued_template_batch_rechecks_changed_targets_but_preserves_frozen_dispatched_target")], ["after a native chunk is dispatched, a profile edit and selected-profile deletion do not change its frozen target","a 504 after PostgreSQL applies the mapping is read back as completed with the exact frozen payload and no replay"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_batch_reports_unchanged_success_and_ineligible_children_together", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["already-matching source completes unchanged without write; queued eligible children and ineligible Grok child appear together with exact terminal state"], (), (DB_FIXTURE,)),
    row("test_account_batch031", "AccountTemplateBatch031Tests", "test_other_template_and_name_changes_do_not_invalidate_queued_batch", [target("queued_template_batch_rechecks_changed_targets_but_preserves_frozen_dispatched_target")], ["renaming the selected profile and editing an unrelated profile leave a queued batch eligible","the exact original selected mapping is dispatched once and read back from PostgreSQL"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),

    # Single account operations (023).
    row("test_account_operations023", "Queue023Tests", "test_busy_queues_other_account_runs_then_exact_http_write", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently")], ["child waits under real parent credential lease, independent account completes, child writes once after unlock"], (), (DB_FIXTURE,)),
    row("test_account_operations023", "Queue023Tests", "test_irrelevant_change_proceeds_real_change_preserves_intent", [target("settings_http_projects_versions_and_emits_exact_manual_write_payloads"), target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently")], ["passive account observations preserve the priority field version while a relevant priority change rotates it","a relevant priority change under the held parent lease invalidates the queued child; the external value is preserved and no stale PUT is sent"], (), (DB_FIXTURE, "tests/contracts/version-vectors.json#/operation_versions_full_synthetic_account")),
    row("test_account_operations023", "Queue023Tests", "test_idempotence_supersession_cancel_and_same_parent", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently")], ["same request ID returns the same job; changed payload conflicts; a new priority supersedes the queued intent; cancellation prevents its write; parent lock blocks child work"], (), (DB_FIXTURE,)),
    row("test_account_operations023", "Queue023Tests", "test_restart_queued_resumes_but_uncertain_write_only_checks", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently"), target("settings_unknown_priority_write_is_checked_without_replay")], ["reopened Operations resumes the persisted queued priority job and dispatches its exact write once","an uncertain priority write becomes checking and then needs_confirmation after readback, with no replay"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_account_operations023", "Queue023Tests", "test_timeout_readback_completes_without_replay_and_save_failure_never_sends", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently"), target("settings_unknown_priority_write_is_checked_without_replay")], ["an applied priority write with a 504 is completed from PostgreSQL readback and its exact PUT is not repeated","a non-applied uncertain priority write becomes needs_confirmation without replay","operation-store save failures before acceptance or dispatch produce no native priority write"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_account_operations023", "Queue023Tests", "test_already_satisfied_short_operation_is_only_verified", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently")], ["an already-satisfied priority operation completes as verified without another upstream write"], (), (DB_FIXTURE,)),
    row("test_account_operations023", "OperationStoreBatchMigrationTests", "test_legacy_single_operation_state_migrates_batch_indexes_without_losing_jobs", [target("operation_store_migrates_legacy_single_job_without_losing_jobs_or_request_indexes")], ["legacy job and request index survive migration; empty batch and batch-request indexes are added"], (), ("server/tests/account_mutation_parity.rs#legacy operation-store fixture",)),

    # Settings and capabilities (035).
    row("test_account_settings035", "SettingsContracts035Tests", "test_numeric_requests_are_strict_and_effective_load_factor_is_automatic", [target("settings_model_config_proxy_clear_sync_and_numeric_validation_use_exact_contracts")], ["concurrency, load_factor, and proxy_id accept both valid endpoints, including i32 and i64 maxima","zero, over-limit, boolean, fractional, string, and overflowing numeric values are rejected by the live route","explicit NULL load_factor is accepted, written as JSON null, and read back as PostgreSQL NULL","effective load factor resolves explicit value, concurrency fallback, and default value from PostgreSQL state"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "SettingsContracts035Tests", "test_settings_versions_are_field_scoped_and_ignore_passive_observations", [target("settings_http_projects_versions_and_emits_exact_manual_write_payloads"), target("all_version_vectors_match_the_pinned_python_contract", RUST_FOUNDATION)], ["priority version ignores passive observations but changes with priority; pinned full-account, parent-child, group-order, and model-config version vectors match Python"], (), (DB_FIXTURE, "tests/contracts/version-vectors.json#/operation_versions_*")),
    row("test_account_settings035", "SettingsContracts035Tests", "test_real_credential_inheritance_and_passthrough_state_controls_capabilities", [target("settings_capabilities_and_human_priority_keep_recovery_intents_independent"), target("settings_http_projects_versions_and_emits_exact_manual_write_payloads")], ["inherited credentials report each model, proxy, and sync capability as non-editable with its exact independent reason","passthrough disables model configuration but retains proxy and model-sync capability with empty reasons"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "SettingsContracts035Tests", "test_model_config_writes_only_local_mapping_and_proxy_can_clear_with_zero", [target("settings_model_config_proxy_clear_sync_and_numeric_validation_use_exact_contracts")], ["model configuration dispatches only credentials.model_mapping, preserves credential extras, proxy clear emits proxy_id=0, and PostgreSQL reads back NULL"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "SettingsContracts035Tests", "test_sync_catalog_posts_only_safe_model_ids_and_never_runs_test_or_quota", [target("settings_model_config_proxy_clear_sync_and_numeric_validation_use_exact_contracts")], ["sync sends one empty-body POST, deduplicates/rejects unsafe model IDs, returns the warning, and never calls test or quota"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "SettingsContracts035Tests", "test_proxy_dto_exposes_no_address_or_authentication_fields", [target("settings_http_projects_versions_and_emits_exact_manual_write_payloads")], ["proxy DTO contains four public fields and omits host, port, username, password, and synthetic credentials"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "OperationsContracts035Tests", "test_busy_settings_operation_remains_queued_then_runs_once", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently")], ["real lock keeps queued child operation pending while unrelated work completes; one write occurs after unlock"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "OperationsContracts035Tests", "test_unchanged_settings_operation_is_verified_without_write", [target("settings_queue_waits_for_parent_lock_then_writes_once_and_supersedes_idempotently")], ["already-satisfied setting reports verified completion with no second PUT"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "OperationsContracts035Tests", "test_timeout_enters_confirmation_and_does_not_replay_write", [target("settings_unknown_priority_write_is_checked_without_replay")], ["a non-applied HTTP 504 enters checking, then needs_confirmation after PostgreSQL readback","the loopback records exactly one PUT after a second tick, proving the unknown write is not replayed"], (), (DB_FIXTURE,)),
    row("test_account_settings035", "OperationsContracts035Tests", "test_priority_does_not_cancel_recovery_or_use_card_intents", [target("settings_capabilities_and_human_priority_keep_recovery_intents_independent")], ["priority, recover, and reset_quota remain separate queued intents; manual priority acceptance sends no write"], (), (DB_FIXTURE,)),

    # Template schema, versions, initialization, and custom catalog (027).
    row("test_account_templates027", "AccountTemplateTests", "test_combine_and_split_native_semantics", [target("template_custom_catalog_caps_crud_and_legacy_empty_state_survive_reload")], ["native fixture profile combines to exact mapping and Rust template view preserves whitelist/mapping split semantics"], (), (ORACLE_TEMPLATE, CONFIG_TEMPLATE)),
    row("test_account_templates027", "AccountTemplateTests", "test_initialization_does_not_guess_source_accounts", [target("template_initialization_uses_only_selected_profiles_once_and_preserves_json_order")], ["no explicit source IDs returns configured=false and performs no inferred initialization"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_templates027", "AccountTemplateTests", "test_initialization_copies_only_three_model_profiles", [target("template_initialization_uses_only_selected_profiles_once_and_preserves_json_order")], ["selected full/degraded rows produce only three profiles; mapping, whitelist, takeover, version keys, permissions, and secret-free view are asserted"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_templates027", "AccountTemplateTests", "test_initialization_is_idempotent_without_nested_lock", [target("template_initialization_uses_only_selected_profiles_once_and_preserves_json_order")], ["same explicit source pair preserves the initialized version; later unrelated source choice cannot overwrite it"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_account_templates027", "AccountTemplateTests", "test_desired_and_application_are_versioned", [target("account_template_custom_crud_mapping_and_request_validation_match_pinned_python", RUST_CONFIG), target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["Python-derived template desired mapping and profile versions are checked; actual batch applies the selected profile to persisted accounts"], (), (CONFIG_TEMPLATE, DB_FIXTURE)),
    row("test_account_templates027", "AccountTemplateTests", "test_template_versions_track_profile_content_but_ignore_names", [target("template_custom_catalog_caps_crud_and_legacy_empty_state_survive_reload"), target("template_http_crud_legacy_put_names_versions_and_deletion_match_python")], ["a name-only edit of the legacy full profile preserves its profile version and updates metadata","editing profile content rotates its profile version and metadata covers the exact profile set"], (), (DB_FIXTURE, CONFIG_TEMPLATE)),
    row("test_account_templates027", "AccountTemplateTests", "test_migrates_legacy_template_state_with_three_builtin_profiles", [target("account_template_schema_upgrade_matches_pinned_python_and_preserves_read_only_or_corrupt_state", RUST_CONFIG)], ["legacy three-profile state normalizes to the pinned schema, persists revision, and retains only the expected custom-template field"], (), (CONFIG_TEMPLATE,)),
    row("test_account_templates027", "AccountTemplateTests", "test_custom_template_crud_uses_dynamic_id_and_preserves_legacy_put", [target("account_template_custom_crud_mapping_and_request_validation_match_pinned_python", RUST_CONFIG), target("template_http_crud_legacy_put_names_versions_and_deletion_match_python")], ["custom template IDs match custom- plus 24 hexadecimal characters; create, update, desired mapping, stale delete, and delete are checked","legacy PUT and dynamic custom-template HTTP paths are exercised through the real Axum API","deletion is confirmed after reload, and invalid/stale requests preserve the saved state"], (), (CONFIG_TEMPLATE,)),
    row("test_account_templates027", "AccountTemplateTests", "test_custom_templates_are_capped_at_32_total", [target("template_custom_catalog_caps_crud_and_legacy_empty_state_survive_reload")], ["29 custom templates plus three built-ins reach 32; over-limit edit returns 409 without changing stored bytes"], (), (DB_FIXTURE,)),
    row("test_account_templates027", "AccountTemplateTests", "test_custom_template_names_reject_whitespace_and_over_limit", [target("account_template_custom_crud_mapping_and_request_validation_match_pinned_python", RUST_CONFIG), target("template_http_crud_legacy_put_names_versions_and_deletion_match_python")], ["blank whitespace names and 41-character names are rejected by both the template API and schema validation","invalid-name requests leave catalog version and stored profile version unchanged"], (), (CONFIG_TEMPLATE,)),
    row("test_account_templates027", "AccountTemplateTests", "test_legacy_template_ids_can_be_renamed_deleted_and_remain_empty_after_reload", [target("template_custom_catalog_caps_crud_and_legacy_empty_state_survive_reload")], ["each legacy profile ID can be renamed/deleted; final template, metadata, and version maps stay empty after reload"], (), (DB_FIXTURE,)),
    row("test_account_templates027", "AccountTemplateTests", "test_legacy_three_item_save_after_template_deletion_requires_upgrade_without_partial_write", [target("template_legacy_save_and_migration_failures_preserve_original_bytes")], ["legacy three-profile save after deletion returns 409 and preserves exact original bytes"], (), ("server/tests/account_mutation_parity.rs#temporary template state",)),
    row("test_account_templates027", "AccountTemplateTests", "test_legacy_migration_save_error_is_propagated", [target("template_legacy_save_and_migration_failures_preserve_original_bytes")], ["blocked migration lock propagates failure and preserves the original legacy state bytes"], (), ("server/tests/account_mutation_parity.rs#temporary template state",)),

    # Manual group management.
    row("test_group_management", "GroupManagementTests", "test_put_uses_scope_target_version_and_preserves_out_of_scope_members", [target("group_management_http_preserves_scope_rejects_stale_targets_and_requires_readback")], ["scoped group PUT merges out-of-scope memberships, reads PostgreSQL back, and sends one exact payload"], (), (DB_FIXTURE,)),
    row("test_group_management", "GroupManagementTests", "test_put_requires_fresh_admin_auth_and_response_excludes_credentials", [target("group_management_auth_schema_deletion_and_error_readback_match_pinned_contract")], ["wrong admin key returns 401 without native PUT; successful result excludes credentials and extra"], (), (DB_FIXTURE,)),
    row("test_group_management", "GroupManagementTests", "test_cross_platform_deleted_scope_and_invalid_target_are_rejected_without_put", [target("group_management_http_preserves_scope_rejects_stale_targets_and_requires_readback"), target("group_management_auth_schema_deletion_and_error_readback_match_pinned_contract")], ["cross-platform, deleted scope, stale target, and invalid empty scope fail before another upstream PUT"], (), (DB_FIXTURE,)),
    row("test_group_management", "GroupManagementTests", "test_account_platform_deleted_account_and_version_conflicts_are_rejected", [target("group_management_auth_schema_deletion_and_error_readback_match_pinned_contract"), target("group_management_http_preserves_scope_rejects_stale_targets_and_requires_readback")], ["unsupported account platform and deleted account are rejected; stale account version returns conflict"], (), (DB_FIXTURE,)),
    row("test_group_management", "GroupManagementTests", "test_timeout_reads_back_applied_membership_without_replaying_put", [target("group_management_auth_schema_deletion_and_error_readback_match_pinned_contract")], ["a loopback 504 after applying the scoped group update is confirmed by PostgreSQL readback","the route returns the merged group IDs and sends exactly one PUT with the exact API key and no credentials"], (), (DB_FIXTURE,)),
    row("test_group_management", "GroupManagementTests", "test_upstream_success_without_readback_is_an_unconfirmed_error_without_replay", [target("group_management_http_preserves_scope_rejects_stale_targets_and_requires_readback")], ["upstream 2xx without membership readback returns 502, preserves old memberships, and is not replayed"], (), (DB_FIXTURE,)),
    row("test_group_management", "GroupManagementTests", "test_request_schema_forbids_extra_fields_and_empty_scope", [target("group_management_auth_schema_deletion_and_error_readback_match_pinned_contract")], ["unknown request keys and empty scope return 422 without upstream writes"], (), (DB_FIXTURE,)),

    # Native group/proxy/template/delete batches.
    row("test_native_batch038", "NativeBatch038Tests", "test_groups_merge_final_payload_and_preserve_scope_outside", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes")], ["native group batch merges out-of-scope groups, shares one exact payload, and reads both accounts back"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_proxy_clear_is_one_native_post_with_zero_only", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes")], ["multi-account proxy clear emits one POST with proxy_id=0 and reads both persisted NULL values back"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_template_batch_writes_only_credentials_model_mapping", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["template batch sends only credentials.model_mapping; source remains unchanged and modified child readback completes"], (), (DB_FIXTURE, ORACLE_TEMPLATE)),
    row("test_native_batch038", "NativeBatch038Tests", "test_more_than_100_accounts_are_chunked_without_replay", [target("native_proxy_scheduler_sends_a_101_account_batch_as_two_exact_chunks")], ["the real background scheduler completes a 101-account batch as exact 100- and 1-account chunks","the two chunks contain every account exactly once and all 101 PostgreSQL rows read back the target"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_native_batch038", "NativeBatch038Tests", "test_busy_account_stays_queued_while_other_account_runs", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states"), target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children")], ["real account lease leaves one child queued while another child writes; releasing it dispatches exactly once"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_unchanged_children_are_verified_without_native_write", [target("native_template_batch_is_atomic_secret_free_idempotent_and_skips_busy_children"), target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["an already-matching template child and an already-matching proxy child complete as unchanged","the unchanged proxy child shares a mixed OpenAI/Grok batch but is excluded from the exact native account_ids"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_partial_result_marks_only_failed_account", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["valid partial native result partitions success and failed IDs; persisted mapping/proxy readback differs only for the reported failed child"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_malformed_native_result_never_claims_success_or_replays", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes")], ["malformed per-account result IDs and mismatched aggregate totals each become needs_confirmation","PostgreSQL retains the original proxy values and a second tick does not replay either bulk write"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_native_batch038", "NativeBatch038Tests", "test_dispatch_persistence_failure_never_writes_native", [target("proxy_batch_acceptance_and_dispatch_persistence_failures_never_send_early")], ["failure to save an accepted proxy batch leaves no batch or child jobs and sends no native request","failure to persist the dispatched state leaves accepted children queued and emits no early bulk write","after recovery the batch sends the exact proxy payload once and PostgreSQL reads back the target"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_native_batch038", "NativeBatch038Tests", "test_timeout_before_write_and_after_write_are_readback_only", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes")], ["a non-applied 504 leaves the old PostgreSQL value and enters needs_confirmation without replay","a 504 after apply completes through PostgreSQL readback and never repeats the exact native request"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_cancelled_child_is_not_sent_in_native_batch", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["cancelled child is excluded from exact bulk account_ids; remaining children complete"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_restart_running_child_becomes_read_only_checking", [target("proxy_batch_restart_checks_a_dispatched_chunk_without_replaying_it")], ["a persisted running proxy child and dispatched chunk reopen as checking","the recovered batch checks PostgreSQL without native replay and finishes needs_confirmation when the old target was not applied"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_native_batch038", "NativeBatch038Tests", "test_delete_requires_confirmation_detaches_managed_and_sends_only_ids", [target("native_delete_confirmation_detaches_managed_accounts_and_rechecks_children")], ["unconfirmed native delete returns 422 before persisting a batch","confirmed mixed-platform delete detaches the managed account, sends only exact account IDs once, and deletes both PostgreSQL rows"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_delete_child_set_change_requires_confirmation_without_native_write", [target("native_delete_confirmation_detaches_managed_accounts_and_rechecks_children")], ["adding a child after delete acceptance changes the queued parent to needs_confirmation with the related-account reason","the delete native request count stays unchanged and the new PostgreSQL child remains present"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_delete_managed_child_set_change_requires_confirmation_without_native_write", [target("native_delete_confirmation_detaches_managed_accounts_and_rechecks_children")], ["adding a newly managed child after delete acceptance changes the managed parent to needs_confirmation","no batch-delete request occurs and the exact managed-account set remains in config"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_inherited_proxy_and_template_children_are_rejected_without_write", [target("proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic")], ["real proxy and template batch routes reject inherited children with failed status and a reason","no native bulk update is sent for either inherited child"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_template_version_change_requires_confirmation", [target("proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic"), target("queued_template_batch_rechecks_changed_targets_but_preserves_frozen_dispatched_target")], ["a stale template-version token is rejected with 409 before batch persistence","editing the selected profile after queue acceptance changes the child to needs_confirmation without writing the account"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_unavailable_proxy_is_rejected_without_native_write", [target("proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic")], ["an inactive synthetic proxy target is marked needs_confirmation before dispatch","the native proxy bulk-update request count remains zero"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_loop_auto_launches_one_native_batch", [target("native_proxy_scheduler_sends_a_101_account_batch_as_two_exact_chunks")], ["the actual runtime background scheduler, without a manual tick, dispatches and completes the accepted native batch","request count and exact chunk contents show each account is written exactly once"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_explicit_route_404_falls_back_once_but_generic_404_does_not_replay", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes")], ["an explicit unsupported-route 404 triggers exactly one per-account fallback PUT with the exact proxy body","a generic 404 becomes needs_confirmation and triggers no fallback or replay"], (), (DB_FIXTURE,)),
    row("test_native_batch038", "NativeBatch038Tests", "test_malformed_account_batch_id_is_rejected_before_persist", [target("proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic")], ["string account IDs, duplicate IDs, malformed fields, empty/oversized account sets, zero target, and extra proxy fields are rejected with 422","all rejected shapes leave persisted batch and job indexes empty"], (), (DB_FIXTURE,)),

    # Proxy batch requests and route (036).
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_request_contract_rejects_bad_shapes_and_duplicate_accounts", [target("proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic")], ["empty, oversized, duplicate, and string-ID batches plus zero/extra-field proxy payloads are rejected before persistence","the invalid requests leave both batch and job indexes empty"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_multiple_platforms_unchanged_and_clear_are_listed_and_written", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes"), target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["mixed OpenAI/Grok changed targets are sorted into one exact native request while an already-matching child completes unchanged without dispatch","proxy clear is sent as proxy_id=0 and both PostgreSQL proxy_id values read back NULL"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_inheritance_deleted_missing_and_invalid_target_are_per_item", [target("proxy_batch_schema_template_and_target_preflight_are_per_item_and_atomic")], ["inherited, deleted, missing, inactive, and nonexistent-target cases produce exact per-item failed/needs_confirmation statuses","preflight rejects account writes before native bulk dispatch"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_partial_proxy_writes_and_cancel_keep_each_child_state", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["valid partial result marks only failed account; cancelled child is omitted from native target; completed peers persist"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_field_conflict_is_confirmed_without_proxy_write", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["reusing a request ID with a changed proxy target returns 409","changing proxy_id under the account lease after acceptance yields needs_confirmation, preserves the live PostgreSQL value, and sends no bulk write"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_busy_queue_retries_once_after_account_becomes_free", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["busy account remains queued while free peer writes, then writes once after actual lease release"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_idempotency_conflict_and_account_order_are_deterministic", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["reordered accounts preserve the same batch and canonical child order; payload reuse conflicts"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_restart_of_checking_never_replays_proxy_write", [target("proxy_batch_restart_checks_a_dispatched_chunk_without_replaying_it")], ["a real proxy batch is reopened with a persisted running job and dispatched native chunk","recovery performs readback only and leaves needs_confirmation without a repeated native POST"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_timeout_after_proxy_write_reads_back_without_replay", [target("native_group_and_proxy_batches_send_exact_targets_and_never_replay_unknown_writes")], ["the loopback applies the exact proxy clear then returns 504; PostgreSQL readback completes the job","a second tick does not replay the single upstream native request"], (), (DB_FIXTURE,)),
    row("test_proxy_batch036", "ProxyBatch036Tests", "test_save_failure_does_not_accept_batch_or_dispatch_proxy", [target("proxy_batch_acceptance_and_dispatch_persistence_failures_never_send_early")], ["failed batch acceptance stores no batch/jobs and sends no native request","failed dispatch-state persistence leaves accepted jobs queued and does not send before the state can be saved","retry after restoring the test-local store lock sends exactly one proxy write and reads it back from PostgreSQL"], (), (DB_FIXTURE, OPERATION_ORACLE)),
    row("test_proxy_batch036", "ProxyBatchRoute036Tests", "test_post_batch_route_returns_202_and_forwards_key", [target("native_proxy_partial_cancel_busy_and_idempotency_preserve_child_states")], ["actual desktop batch route returns 202; loopback Sub2API receiver records the exact x-api-key and proxy body"], (), (DB_FIXTURE,)),

    # Proxy diagnostics (037).
    row("test_proxy_diagnostics037", "ProxyDiagnostics037Tests", "test_safe_proxy_test_forwards_native_action_with_bounded_timeout", [target("proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python"), target("proxy_diagnostic_waits_for_the_real_90_second_timeout_without_replay")], ["the actual diagnostic route forwards one empty-body POST with the exact key and matches the pinned safe-result oracle","the real configured 90-second timeout returns within the bounded test window and does not replay the request","private proxy address, user, password, and token fields are absent from the public response"], (), (DB_FIXTURE, ORACLE_PROXY)),
    row("test_proxy_diagnostics037", "ProxyDiagnostics037Tests", "test_quality_result_keeps_safe_items_and_checked_at_without_credentials", [target("proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python")], ["quality score, checked_at, item fields and exact sanitized result match pinned Python; credential extras are absent"], (), (DB_FIXTURE, ORACLE_PROXY)),
    row("test_proxy_diagnostics037", "ProxyDiagnostics037Tests", "test_same_proxy_and_action_coalesces_while_different_action_is_independent", [target("proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python")], ["two concurrent same proxy/action clicks share one upstream request while quality-check receives its own request"], (), (DB_FIXTURE, ORACLE_PROXY)),
    row("test_proxy_diagnostics037", "ProxyDiagnostics037Tests", "test_timeout_returns_without_replaying_the_native_request", [target("proxy_diagnostic_waits_for_the_real_90_second_timeout_without_replay")], ["the loopback remains gated until the actual 90-second diagnostic timeout expires","the route returns 504 with one upstream POST and remains unreplayed after the gate is released"], (), (DB_FIXTURE, ORACLE_PROXY)),
    row("test_proxy_diagnostics037", "ProxyDiagnostics037Tests", "test_native_404_is_reported_as_an_unsupported_diagnostic", [target("proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python")], ["native 404 becomes the unsupported-diagnostic 502 and uses one native request"], (), (DB_FIXTURE, ORACLE_PROXY)),
    row("test_proxy_diagnostics037", "ProxyDiagnosticsRoute037Tests", "test_desktop_routes_authenticate_and_forward_quality_check", [target("proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python"), target("proxy_diagnostic_waits_for_the_real_90_second_timeout_without_replay")], ["the desktop quality route forwards the exact admin key and returns Python-derived sanitized quality output","a bad admin key returns 401 before any upstream call; the diagnostic timeout path also authenticates before dispatch"], (), (DB_FIXTURE, ORACLE_PROXY)),

    # Loopback-native proxy diagnostics (037).
    row("test_proxy_native_http037", "NativeProxyHttpTests", "test_native_paths_headers_and_whitelist_arrive_at_real_receiver", [target("proxy_native_http_routes_headers_and_whitelists_match_python_oracle_at_loopback")], ["the real desktop routes send exactly one POST each to /test and /quality-check with an empty JSON body and the configured admin key","the test result matches fixed-SHA Python safe_result output with latency 13 and redacts the native proxy URL and credentials","the quality result matches fixed-SHA Python safe_result output with score 80 and a challenge item"], (), (DB_FIXTURE, "tests/contracts/account-mutation/fixtures.json#/python_oracle/proxy_native_http")),
    row("test_proxy_native_http037", "NativeProxyHttpTests", "test_inflight_clicks_share_one_actual_request_and_failure_is_not_replayed", [target("proxy_diagnostics_route_auth_coalescing_sanitization_and_unsupported_status_match_python")], ["concurrent clicks coalesce to one native request; failing native status returns error with one additional non-replayed request"], (), (DB_FIXTURE, ORACLE_PROXY)),

    # Desktop account actions.
    row("test_desktop_actions", "DesktopActionContractTests", "test_model_options_use_union_of_current_group_allowlists", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["live group union filters provider catalogue in exact Python-derived order and deduplicates duplicate model IDs"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopActionContractTests", "test_exact_model_test_options_ignore_wildcards_media_and_blanks", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["approved 0.1.47 candidate rule and 0.1.48 directory fix: read the current account provider with its effective credentials and proxy, bypass the mapped admin catalog; absent IDs, wildcards and media are excluded; original Python oracle is preserved"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopActionContractTests", "test_priority_is_strict_and_bounded", [target("settings_model_config_proxy_clear_sync_and_numeric_validation_use_exact_contracts")], ["the live direct-priority route rejects booleans and negative values","priority zero and i32::MAX are accepted and read back from PostgreSQL with one exact PUT each"], (), (DB_FIXTURE,)),
    row("test_desktop_actions", "DesktopActionContractTests", "test_test_request_accepts_missing_or_legacy_confirmation", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["missing and legacy true confirmation both work; unknown mode returns 422 before native request"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopActionContractTests", "test_grok_batch_accepts_only_fresh_complete_billing", [target("desktop_grok_quota_refresh_accepts_only_fresh_complete_billing")], ["fresh, one-second-stale, and partial Grok billing cases use fixed-SHA Python oracle inputs and expected outcomes","the actual quota-refresh route makes one exact loopback usage request per OAuth Grok account with no credentials in its response"], (), (DB_FIXTURE, "tests/contracts/account-mutation/fixtures.json#/python_oracle/desktop_grok_billing")),
    row("test_desktop_actions", "DesktopStreamTests", "test_models_filters_using_live_group_allowlist_union", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["live group allowlist union filters the provider catalog; actual API performs one upstream model request"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopStreamTests", "test_model_test_candidates_are_full_exact_group_union_without_catalog_intersection", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["approved 0.1.47 difference: even complete allowlists require the current-account directory; no absent catalog IDs are added"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopStreamTests", "test_model_test_candidates_fall_back_when_no_group_or_allowlist_disabled", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["no-group and disabled-allowlist requests both use the current-account directory and filter to text models"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopStreamTests", "test_model_test_submission_rechecks_exact_allowlist", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["approved 0.1.47 difference: candidate is rechecked against the current-account directory; whitelist-only, wildcard and media IDs are denied"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
    row("test_desktop_actions", "DesktopStreamTests", "test_real_sse_keeps_whitespace_and_releases_lock_without_confirmation", [target("desktop_model_options_live_allowlists_and_sse_payload_match_python_oracle")], ["exact Python SSE content chunks retain whitespace/newlines; test can be repeated without confirmation after lock release"], (), (DB_FIXTURE, ORACLE_DESKTOP)),
]


def assertion_anchors(source: str, method: ast.FunctionDef | ast.AsyncFunctionDef) -> list[dict[str, Any]]:
    values: list[dict[str, Any]] = []
    seen: set[tuple[int, str]] = set()
    for node in ast.walk(method):
        is_assert = isinstance(node, ast.Assert)
        if isinstance(node, ast.Call):
            function = node.func
            is_assert = is_assert or (
                isinstance(function, ast.Attribute) and function.attr.startswith("assert")
            ) or (isinstance(function, ast.Name) and function.id.startswith("assert"))
        if not is_assert:
            continue
        excerpt = ast.get_source_segment(source, node) or "assertion"
        excerpt = re.sub(r"\s+", " ", excerpt).strip()
        key = (node.lineno, excerpt)
        if key in seen:
            continue
        seen.add(key)
        values.append({"line": node.lineno, "text": excerpt[:280]})
    return sorted(values, key=lambda item: (item["line"], item["text"]))


def build_migration_fragment(repo: Path, fixtures: dict[str, Any]) -> dict[str, Any]:
    declared: dict[str, dict[str, Any]] = {}
    for module, class_name, method, value in MAPPING_ROWS:
        key = f"{module}.{class_name}.{method}"
        if key in declared:
            raise RuntimeError(f"duplicate legacy migration row: {key}")
        declared[key] = value

    mappings: list[dict[str, Any]] = []
    expected_modules: set[str] = set(LEGACY_MODULES)
    for module, expected_count in LEGACY_MODULES.items():
        path = f"tests/{module}.py"
        source = git_show(repo, path).decode("utf-8")
        tree = ast.parse(source, filename=path)
        functions: list[tuple[str, ast.FunctionDef | ast.AsyncFunctionDef]] = []
        for class_node in tree.body:
            if not isinstance(class_node, ast.ClassDef):
                continue
            for method in class_node.body:
                if isinstance(method, (ast.FunctionDef, ast.AsyncFunctionDef)) and method.name.startswith("test_"):
                    functions.append((class_node.name, method))
        if len(functions) != expected_count:
            raise RuntimeError(f"legacy test count changed in pinned {path}")
        for class_name, method in functions:
            key = f"{module}.{class_name}.{method.name}"
            coverage = declared.pop(key, None)
            if coverage is None:
                raise RuntimeError(f"missing precise migration row: {key}")
            mappings.append(
                {
                    "legacy_id": f"py:tests.{module}:{class_name}.{method.name}",
                    "source": {
                        "path": path,
                        "sha256": sha256_bytes(git_show(repo, path)),
                        "line_start": method.lineno,
                        "line_end": method.end_lineno,
                        "assertion_anchors": assertion_anchors(source, method),
                    },
                    "status": "partial" if coverage["uncovered_assertions"] else "full",
                    **coverage,
                }
            )
    if declared:
        raise RuntimeError("migration rows do not exist at the pinned source: " + ", ".join(sorted(declared)))
    if not expected_modules:
        raise RuntimeError("legacy module manifest is empty")
    summary = {
        status: sum(item["status"] == status for item in mappings)
        for status in ("full", "partial")
    }
    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "test_count": len(mappings),
        "status_counts": summary,
        "mappings": sorted(mappings, key=lambda item: item["legacy_id"]),
    }


def run_rust(args: argparse.Namespace) -> int:
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(ROOT / "scripts"))
    try:
        from server_http_contract import TemporaryPostgres
        from server_operation_contract import safe_environment
    except ImportError as exc:
        raise RuntimeError("cannot import the existing temporary PostgreSQL runner") from exc

    result: dict[str, Any] = {
        "status": "failed",
        "reference_sha": REFERENCE_SHA,
        "test_source_sha256": sha256_bytes((ROOT / "server/tests/account_mutation_parity.rs").read_bytes()),
        "fixture_sha256": sha256_bytes(FIXTURE_PATH.read_bytes()),
        "migration_sha256": sha256_bytes(MIGRATION_PATH.read_bytes()),
        "command": "cargo test --manifest-path server/Cargo.toml --locked --offline --test account_mutation_parity -- --test-threads=1 --nocapture",
    }
    postgres: Any | None = None
    with tempfile.TemporaryDirectory(prefix="server-account-mutation-contract-040-") as name:
        scratch = Path(name)
        try:
            postgres = TemporaryPostgres(scratch, args.postgres_bin_dir)
            postgres.start()
            env = safe_environment(postgres.url, scratch)
            completed = subprocess.run(
                [
                    "cargo",
                    "test",
                    "--manifest-path",
                    "server/Cargo.toml",
                    "--locked",
                    "--offline",
                    "--test",
                    "account_mutation_parity",
                    "--",
                    "--test-threads=1",
                    "--nocapture",
                ],
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            output = (completed.stdout or "") + "\n" + (completed.stderr or "")
            names = re.findall(r"^\s*test ([A-Za-z0-9_:]+) \.\.\. (ok|FAILED)$", output, re.M)
            summaries = re.findall(
                r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out",
                output,
            )
            summary = None
            if summaries:
                status, passed, failed, ignored, measured, filtered = summaries[-1]
                summary = {
                    "status": "passed" if status == "ok" else "failed",
                    "passed": int(passed),
                    "failed": int(failed),
                    "ignored": int(ignored),
                    "measured": int(measured),
                    "filtered_out": int(filtered),
                }
            result.update(
                {
                    "exit_code": completed.returncode,
                    "tests": {
                        "passed": [name for name, status in names if status == "ok"],
                        "failed": [name for name, status in names if status == "FAILED"],
                        "summary": summary,
                    },
                }
            )
            if completed.returncode != 0:
                diagnostics = []
                for line in output.splitlines():
                    clean = re.sub(r"\x1b\[[0-9;]*m", "", line).strip()
                    if clean.startswith("error") or "panicked at" in clean or clean.startswith("left:") or clean.startswith("right:"):
                        diagnostics.append(clean[:320])
                result["diagnostics"] = diagnostics[:80]
            result["status"] = "passed" if completed.returncode == 0 and summary and summary["status"] == "passed" else "failed"
        except BaseException as exc:
            result["error"] = type(exc).__name__
            result["exit_code"] = 1
        finally:
            if postgres is not None and getattr(postgres, "started", False):
                try:
                    postgres.close()
                    result["postgres_stopped"] = True
                except BaseException as exc:
                    result["postgres_stopped"] = False
                    result["cleanup_error"] = type(exc).__name__
    result["temporary_directory_removed"] = not scratch.exists()
    if result.get("cleanup_error") or not result.get("temporary_directory_removed"):
        result["status"] = "failed"
        result["exit_code"] = result.get("exit_code") or 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return int(result.get("exit_code", 1))
def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="write the fixture and migration-fragment JSON files")
    parser.add_argument("--run-rust", action="store_true", help="run only the account_mutation_parity target with temporary PostgreSQL")
    parser.add_argument("--postgres-bin-dir", help="directory containing initdb and pg_ctl")
    args = parser.parse_args()
    check_reference(ROOT)
    fixtures = python_oracles(ROOT)
    migration = build_migration_fragment(ROOT, fixtures)
    if args.write:
        CONTRACT_DIR.mkdir(parents=True, exist_ok=True)
        FIXTURE_PATH.write_text(json.dumps(fixtures, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        MIGRATION_PATH.write_text(json.dumps(migration, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.run_rust:
        if not FIXTURE_PATH.is_file() or json.loads(FIXTURE_PATH.read_text(encoding="utf-8")) != fixtures:
            raise RuntimeError("account-mutation fixtures are stale; regenerate with --write first")
        return run_rust(args)
    if not args.write:
        sys.stdout.write(json.dumps({"fixtures": fixtures, "migration_fragment": migration}, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    else:
        print(json.dumps({"status": "written", "fixture": str(FIXTURE_PATH.relative_to(ROOT)), "migration_fragment": str(MIGRATION_PATH.relative_to(ROOT)), "legacy_tests": migration["test_count"]}, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
