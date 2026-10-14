#!/usr/bin/env python3
"""Capture pure-rule inputs and results while running the pinned regression tests."""
from __future__ import annotations

import argparse
import copy
from contextlib import ExitStack
import datetime as dt
import importlib
import inspect
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from server_credit_contract import safe_environment
from server_http_contract import REFERENCE_SHA, ROOT

SELECTION = {
    "tests.test_key_fallback": {
        "OAuthClassificationTests": None,
        "GrokFallbackTests": ["test_grok_blocking_states_and_unknown_values"],
    },
    "tests.test_quota_snapshot": {"PassiveQuotaTests": None},
    "tests.test_model_catalog": {"RuleTests": None},
    "tests.test_codex_identity": {"CodexIdentityTests": None},
    "tests.test_recovery_config030": {"AutoResetProjection030Tests": None},
    "tests.test_auto_reset": {"ResetCreditDiagnosticTests": [
        "test_native_business_codes_are_preserved_without_extra_response_fields",
        "test_only_whitelisted_status_business_code_and_boolean_fields_are_retained",
        "test_unknown_business_code_is_replaced_and_boolean_reset_count_is_omitted",
    ]},
    "tests.test_auto_reset038": {"AutoReset038ObservationTests": [
        "test_observation_from_usage_requires_fresh_explicit_matching_card_data",
    ]},
    "tests.test_usage_query": {"OAuthUsageTests": {"except": [
        "test_active_usage_uses_x_api_key_without_bearer_header",
    ]}},
    "tests.test_oauth_monitor": {
        "OAuthMonitorSchedulingTests": None,
        "OAuthRecoveryGateTests": None,
    },
    "tests.test_events030": {"EventEvidence030Tests": None},
    "tests.test_desktop_api": {"DesktopEvidenceTests": {"except": [
        "test_invalidation_never_waits_for_an_inflight_snapshot",
        "test_inflight_snapshot_cannot_revive_invalidated_cache",
        "test_errors_use_account_filter_and_cursor",
    ]}},
}
FUNCTIONS = {
    "classify_oauth_account", "classify_grok_oauth_account", "desired_key_schedulable",
    "latest_completed_oauth_result", "oauth_quota_summary_from_result", "latest_openai_result",
    "usage_windows", "build_monitor_candidates", "merge", "validate_overrides", "transform",
    "admitted", "import_fields", "targets", "composite_target", "codex_identity",
    "standard_codex_user_agent", "synced_codex_user_agent", "project_state",
    "receipt_diagnostics", "observation_from_usage",
    "parse_iso_datetime", "oauth_reset_at", "oauth_quota_from_usage_data",
    "oauth_windows_by_key", "oauth_recovery_transition", "_retry_at",
    "automatic_recovery_eligible", "recovery_block_signature",
    "recovery_block_change_is_safe", "account_recovery_confirmed",
    "error_evidence.match_message", "error_evidence.is_local_throttle",
    "account_dto", "group_dtos", "error_dto", "safe_error_body",
}


def serial(value):
    if isinstance(value, dt.datetime):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(key): serial(item) for key, item in value.items()}
    if isinstance(value, (set, frozenset)):
        return [serial(item) for item in sorted(value)]
    if isinstance(value, (tuple, list)):
        return [serial(item) for item in value]
    return value


def collect():
    cases = []
    results = []
    current = ""

    def traced(name, function):
        def call(*args, **kwargs):
            bound = inspect.signature(function).bind(*args, **kwargs)
            bound.apply_defaults()
            captured = copy.deepcopy(bound.arguments)
            if name == "account_dto":
                # Rust's database adapter preserves Python str(datetime) for
                # version hashes; the public DTO separately emits ISO dates.
                captured["row"] = {key: str(value) if isinstance(value, dt.datetime) else value
                                   for key, value in captured["row"].items()}
            arguments = serial(captured)
            record = {"function": name, "legacy_test_id": current}
            try:
                json.dumps(arguments, allow_nan=False)
                record["arguments"] = arguments
            except ValueError:
                record["invalid_json_arguments"] = json.dumps(arguments, ensure_ascii=False)
            before = serial(copy.deepcopy(bound.arguments))
            try:
                value = function(*args, **kwargs)
            except ValueError:
                record["error"] = "invalid_value"
                cases.append(record)
                raise
            record["expected"] = serial(value)
            record["input_unchanged"] = before == serial(bound.arguments)
            cases.append(record)
            return value
        return call

    for module_name, classes in SELECTION.items():
        module = importlib.import_module(module_name)
        with ExitStack() as patches:
            for name in FUNCTIONS:
                owner = module
                parts = name.split(".")
                for part in parts[:-1]:
                    owner = getattr(owner, part, None)
                function = getattr(owner, parts[-1], None)
                if callable(function):
                    patches.enter_context(patch.object(owner, parts[-1], traced(name, function)))
            for class_name, selected in classes.items():
                cls = getattr(module, class_name)
                names = selected or unittest.defaultTestLoader.getTestCaseNames(cls)
                if isinstance(selected, dict):
                    names = [name for name in unittest.defaultTestLoader.getTestCaseNames(cls)
                             if name not in selected["except"]]
                for method in names:
                    current = f"py:{module_name}:{class_name}.{method}"
                    result = unittest.TestResult()
                    cls(method).run(result)
                    if not result.wasSuccessful():
                        raise RuntimeError(f"pinned pure-rule oracle failed: {current}")
                    results.append(current)
    return {"reference_sha": REFERENCE_SHA, "legacy_tests": results, "cases": cases}


def write_contracts(fixture):
    mappings = []
    functions = []
    for index, identifier in enumerate(fixture["legacy_tests"]):
        module, qualified = identifier.split(":")[1:]
        name = (module.removeprefix("tests.test_") + "_" + qualified.replace(".", "_")).lower()
        invocation = f"check({json.dumps(identifier)});"
        functions.append(f"#[test]\nfn {name}() {{\n    {invocation}\n}}\n")
        helpers = [
            {"name": "check", "assertions": [
                "assert!(!cases.is_empty(),", "assert_same(",
                "assert!(actual.is_err(),", "assert_eq!(args, case[\"arguments\"]);",
            ]},
            {"name": "assert_same", "assertions": [
                "assert_eq!(a.len(), b.len(),", "assert!(a.contains_key(key),",
                "_ => assert_eq!(actual, expected,", "(a - b).abs() <= 1e-10 * b.abs().max(1.0)",
            ]},
        ]
        if "autoresetprojection" in name:
            helpers.append({"name": "execute", "assertions": [
                "assert_eq!(std::fs::read(&path).unwrap(), before);",
                "OAuthState::new(path, false).snapshot()?[\"scheduler\"][\"31\"][\"auto_reset_credit\"],",
            ]})
        paths = [f"legacy_tests.{index}"]
        paths.extend(f"cases.{i}" for i, case in enumerate(fixture["cases"]) if case["legacy_test_id"] == identifier)
        mappings.append({
            "legacy_test_id": identifier, "status": "mapped",
            "target": {"kind": "rust_test", "path": "server/tests/pinned_rule_traces.rs",
                       "test_name": name, "assertions": ["check(", json.dumps(identifier)], "helper_assertions": helpers,
                       "fixtures": [{"path": "tests/contracts/rule-traces/fixtures.json", "json_paths": paths}]},
            "coverage_note": "在固定 0.1.39 Git 源码中运行该旧测试并逐次捕获真实规则输入与结果；Rust 对全部捕获调用比较完整对象、数组顺序、数值及非法值拒绝，保持输入不变。非有限数在 JSON 输入边界拒绝；用卡投影额外验证落盘历史与旧状态重读不变。",
        })
    target = ROOT / "server/tests/pinned_rule_traces.rs"
    marker = "// The individual tests below correspond to pinned Python test identities."
    source = target.read_text().split(marker)[0] + marker + "\n\n" + "\n".join(functions)
    target.write_text(source)
    subprocess.run(["rustfmt", "--edition", "2021", str(target)], check=True)
    fragment = {"schema_version": 1, "reference_sha": REFERENCE_SHA, "mappings": mappings}
    (ROOT / "tests/contracts/rule-traces/migration-fragment.json").write_text(
        json.dumps(fragment, ensure_ascii=False, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", action="store_true")
    args = parser.parse_args()
    if args.worker:
        sys.path.insert(0, str(Path.cwd()))
        json.dump(collect(), sys.stdout, ensure_ascii=False, indent=2, sort_keys=True, allow_nan=False)
        return
    archive = subprocess.run(["git", "archive", REFERENCE_SHA, "app", "tests"],
                             cwd=ROOT, check=True, capture_output=True).stdout
    with tempfile.TemporaryDirectory(prefix="sub2ops-rule-traces-") as directory:
        reference = Path(directory)
        with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
            bundle.extractall(reference, filter="data")
        env = safe_environment("postgresql://fixture:fixture@127.0.0.1:1/fixture", reference)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(reference)
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--worker"],
                                cwd=reference, env=env, capture_output=True, text=True)
        if result.returncode:
            detail = (result.stdout + result.stderr).replace(str(reference), "<reference>").replace(str(ROOT), "<repository>")
            raise RuntimeError(detail)
        fixture = json.loads(result.stdout)
    destination = ROOT / "tests/contracts/rule-traces/fixtures.json"
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True, allow_nan=False) + "\n")
    write_contracts(fixture)
    print(json.dumps({"legacy_tests": len(fixture["legacy_tests"]), "vectors": len(fixture["cases"])}))


if __name__ == "__main__":
    main()
