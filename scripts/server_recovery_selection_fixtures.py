#!/usr/bin/env python3
"""Export recovery-selection contracts from the fixed production revision."""
from __future__ import annotations

import argparse
import asyncio
import copy
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

from server_credit_contract import safe_environment
from server_http_contract import REFERENCE_SHA, ROOT


def collect():
    from app.config_service import ConfigService
    from app.recovery_policy import CONNECTION_IDS as C, MODEL_IDS as M, SEEN_IDS as S
    from tests.test_recovery_selection034 import Runtime, oauth_account

    loader = unittest.TestLoader()
    suite = unittest.TestSuite()
    names = []
    for class_name in (
        "tests.test_recovery_config030.RecoveryConfig030Tests",
        "tests.test_recovery_config030.RecoveryMigration030Tests",
        "tests.test_recovery_selection034.RecoverySelection034Tests",
    ):
        for test in loader.loadTestsFromName(class_name):
            if test._testMethodName == "test_monitor_run_once_refreshes_inventory_without_network_when_recovery_is_disabled":
                continue
            names.append(test.id())
            suite.addTest(test)
    report = unittest.TextTestRunner(stream=io.StringIO()).run(suite)
    if not report.wasSuccessful():
        raise RuntimeError("pinned recovery selection assertions failed")
    assert len(names) == 16

    cases = []

    def scenario(name, raw, steps):
        with tempfile.TemporaryDirectory(prefix="selection-case-") as directory:
            runtime = Runtime(Path(directory), copy.deepcopy(raw))
            service = ConfigService(runtime)
            initial = service.snapshot("oauth")["oauth"]
            expected = []
            for step in steps:
                if step["action"] == "reconcile":
                    service.reconcile_recovery_accounts(step["rows"])
                else:
                    revision = service.snapshot("oauth")["oauth"]["revision"]
                    asyncio.run(service.save("oauth", step["changes"], "fixture-user", revision))
                result = runtime.oauth_config_file()
                result.pop("updated_at", None)
                result.pop("updated_by", None)
                expected.append(result)
        cases.append({"name": name, "raw": raw, "initial": initial,
                      "steps": steps, "expected": expected})

    def reconcile(*rows):
        return {"action": "reconcile", "rows": list(rows)}

    a, b, c, d = (oauth_account(i) for i in (31, 32, 39, 40))
    scenario("legacy", {"oauth_daily_test_time": "06:15"}, [
        reconcile(a, b, oauth_account(33, deleted_at="2026-10-09T04:00:00+00:00"),
                  oauth_account(34, platform="grok"), oauth_account(35, type="apikey"),
                  oauth_account(36, parent_account_id=31)), reconcile(a, b, c),
    ])
    for name, selected in (("explicit_empty", {C: [], M: []}),
                           ("explicit_selected", {C: [31], M: [32]})):
        scenario(name, selected, [reconcile(a, b, oauth_account(33, platform="grok"))])
    scenario("sticky", {C: [31, 32], M: [], S: [31, 32]}, [
        reconcile(a, b, c), {"action": "save", "changes": {C: [32, 39], M: []}},
        reconcile(a, b, c), reconcile(b, c), reconcile(a, b, c),
    ])
    scenario("model_mode", {C: [31, 32], M: [], S: [31, 32]}, [
        {"action": "save", "changes": {C: [32], M: [31]}}, reconcile(a, b),
    ])
    scenario("partial_save", {C: [31], M: [32]}, [
        {"action": "save", "changes": {"oauth_daily_test_time": "06:15"}},
        {"action": "save", "changes": {C: [32], M: [31]}},
        {"action": "save", "changes": {C: [], M: []}},
    ])
    scenario("stale_revision", {C: [31, 32], M: [], S: [31, 32]}, [
        reconcile(a, b, c), {"action": "save", "changes": {"oauth_daily_test_time": "08:00"}},
    ])
    scenario("concurrent", {C: [31, 32], M: [], S: [31, 32]}, [
        reconcile(a, b, c), reconcile(a, b, d),
        {"action": "save", "changes": {"oauth_daily_test_time": "07:30"}},
    ])
    with tempfile.TemporaryDirectory(prefix="selection-default-") as directory:
        runtime = Runtime(Path(directory), {})
        defaults = ConfigService(runtime).snapshot("oauth")["oauth"]
        defaults.pop("revision")
    return {"reference_sha": REFERENCE_SHA, "legacy_assertions_passed": names,
            "defaults": defaults, "cases": cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", action="store_true")
    args = parser.parse_args()
    if args.worker:
        sys.path[:0] = [str(Path.cwd()), str(Path.cwd() / "tests")]
        json.dump(collect(), sys.stdout, ensure_ascii=False, sort_keys=True, allow_nan=False)
        return
    archive = subprocess.run(["git", "archive", REFERENCE_SHA, "app", "tests"],
                             cwd=ROOT, check=True, capture_output=True).stdout
    with tempfile.TemporaryDirectory(prefix="sub2ops-selection-oracle-") as directory:
        reference = Path(directory)
        with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
            bundle.extractall(reference, filter="data")
        env = safe_environment("postgresql://fixture:fixture@127.0.0.1:1/fixture", reference)
        env.update(PYTHONDONTWRITEBYTECODE="1", PYTHONPATH=str(reference))
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--worker"],
                                cwd=reference, env=env, capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError((result.stdout + result.stderr).replace(str(reference), "<reference>")
                               .replace(str(ROOT), "<repository>"))
        fixture = json.loads(result.stdout)
    path = ROOT / "tests/contracts/recovery-selection/fixtures.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(fixture, ensure_ascii=False, sort_keys=True, indent=2) + "\n")
    print(json.dumps({"cases": len(fixture["cases"]), "legacy_tests": len(fixture["legacy_assertions_passed"]),
                      "reference_sha": REFERENCE_SHA}))


if __name__ == "__main__":
    main()
