#!/usr/bin/env python3
"""Obtain Key fallback dispatch oracles from the pinned production implementation."""
from __future__ import annotations

import argparse
import copy
import datetime as dt
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

from server_credit_contract import safe_environment
from server_http_contract import REFERENCE_SHA, ROOT


def serial(value):
    if isinstance(value, dt.datetime):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(key): serial(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [serial(item) for item in value]
    return value


def collect():
    from tests.test_key_fallback import NOW, oauth_row, key_row, success_result, failure_result, make_controller

    unavailable = {"oauth_results": {"1": failure_result("http_401")}}
    available = {"oauth_results": {"1": success_result()}}
    cases = []

    def add(name, *, oauth=None, grok=None, keys=None, snapshot=None, coexist=(), enabled=True,
            ids=None, persisted_ids=None, fail=(), gate=(), change_after_first=None, busy=False):
        oauth = [oauth_row(1)] if oauth is None else oauth
        original_oauth = copy.deepcopy(oauth)
        keys = [key_row(4), key_row(5, schedulable=True)] if keys is None else keys
        original = copy.deepcopy(keys)
        oauth.extend(row for row in keys if row["type"] == "oauth" and row["platform"] == "openai")
        snapshot = copy.deepcopy(available if snapshot is None else snapshot)
        managed = list(ids) if ids is not None else [row["id"] for row in keys]
        calls = []
        rows = {row["id"]: row for row in keys}

        def runner(account_id, schedulable, **kwargs):
            calls.append({"id": account_id, "schedulable": schedulable,
                          "timeout": kwargs.get("timeout_seconds")})
            if len(calls) == 1 and change_after_first:
                for aid, patch in change_after_first.items():
                    rows[aid].update(copy.deepcopy(patch))
                    if rows[aid]["type"] == "oauth" and rows[aid]["platform"] == "openai":
                        oauth.append(rows[aid])
            if account_id in fail:
                return {"success": False, "error_code": "http_503"}
            rows[account_id]["schedulable"] = schedulable
            return {"success": True}

        with tempfile.TemporaryDirectory(prefix="fallback-case-") as directory:
            controller, _, _ = make_controller(
                Path(directory), oauth_rows=oauth, grok_oauth_rows=grok or [], key_rows=keys,
                snapshot=snapshot, runner=runner, busy=busy,
                account_reader=lambda _db, aid: copy.deepcopy(rows.get(aid)),
            )
            # Save while every selected row is a live supported key; dispatch
            # revalidation is exercised after these test fixture mutations.
            live = copy.deepcopy(rows)
            for row in keys:
                row["deleted_at"] = None
                row["type"] = "apikey"
                if row["platform"] not in ("openai", "grok"):
                    row["platform"] = "openai"
            controller.save_user_config(openai_enabled=enabled, grok_enabled=enabled,
                                        managed_account_ids=managed, coexist_account_ids=list(coexist),
                                        user="fixture-user")
            config = json.loads(Path(controller.settings.key_fallback_config_path).read_text())
            if persisted_ids is not None:
                config["managed_account_ids"] = list(persisted_ids)
                Path(controller.settings.key_fallback_config_path).write_text(json.dumps(config))
            for aid, row in live.items():
                rows[aid].clear()
                rows[aid].update(row)
            controller.detection_gate = lambda aid: aid in gate
            reports = []
            for _ in range(2):
                report = controller.run_once(now=NOW)
                reports.append({key: report.get(key) for key in (
                    "skipped", "desired", "changed_ids", "failed_ids", "reason",
                    "oauth_states", "grok_oauth_states", "desired_by_platform") if key in report})
        config.pop("updated_at", None)
        cases.append(serial({"name": name, "oauth": original_oauth, "grok": grok or [], "keys": original,
                             "snapshot": snapshot, "busy": busy, "config": config,
                             "gate": list(gate), "fail": list(fail),
                             "change_after_first": change_after_first or {},
                             "reports": reports, "calls": calls}))

    add("all_unavailable", snapshot=unavailable, keys=[key_row(4), key_row(5,schedulable=True),key_row(6)], ids=[4,5])
    add("any_available", oauth=[oauth_row(1),oauth_row(2,schedulable=False)],
        snapshot={"oauth_results":{"1":success_result(),"2":failure_result("http_401")}})
    add("unknown_network", snapshot={"oauth_results": {"1": failure_result("network_error")}})
    add("unknown_empty", oauth=[], snapshot={})
    add("unknown_stale", snapshot={"oauth_results": {"1": success_result(queried_at=NOW-dt.timedelta(hours=2))}})
    add("mixed_unknown", oauth=[oauth_row(1), oauth_row(2)], snapshot=unavailable)
    add("partial_failure", snapshot=unavailable, keys=[key_row(4), key_row(5), key_row(6),
        key_row(7,platform="grok"),key_row(8,type="oauth")], ids=[4,5,6],
        persisted_ids=[4,5,6,7,8,99], fail=[5])
    add("dispatch_filters", snapshot=unavailable, keys=[key_row(4), key_row(5, status="disabled"),
        key_row(6, platform="anthropic"), key_row(8, deleted_at=NOW),
        key_row(9, schedulable=True)], ids=[4,5,6,8,9])
    add("live_revalidation", snapshot=unavailable, keys=[key_row(i) for i in (4,5,6,7)],
        change_after_first={5:{"deleted_at":NOW},6:{"type":"oauth"},7:{"schedulable":True}})
    add("opposite_platforms", grok=[oauth_row(2, platform="grok", schedulable=False)],
        keys=[key_row(4, schedulable=True),key_row(5, platform="grok")])
    add("grok_openai_busy", grok=[oauth_row(2, platform="grok", schedulable=False)],
        keys=[key_row(4),key_row(5, platform="grok")], busy=True)
    add("grok_unknown", grok=[oauth_row(2, platform="grok")], oauth=[], keys=[key_row(5, platform="grok")])
    add("disabled", enabled=False)
    add("empty_selection", ids=[])
    add("coexist_available", coexist=[4])
    add("coexist_unknown", coexist=[4], snapshot={"oauth_results": {"1": failure_result("network_error")}})
    add("coexist_missing", coexist=[4], snapshot={"oauth_results":{}})
    add("coexist_stale", coexist=[4], snapshot={"oauth_results":{"1":success_result(queried_at=NOW-dt.timedelta(hours=2))}})
    add("coexist_unavailable", coexist=[4], snapshot=unavailable, keys=[key_row(4), key_row(5)])
    add("coexist_held", coexist=[4,5], keys=[key_row(4,status="disabled"),key_row(5)], gate=[5])
    return {"reference_sha": REFERENCE_SHA, "now": NOW.isoformat(), "cases": cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", action="store_true")
    args = parser.parse_args()
    if args.worker:
        sys.path.insert(0, str(Path.cwd()))
        json.dump(collect(), sys.stdout, ensure_ascii=False, sort_keys=True, allow_nan=False)
        return
    archive = subprocess.run(["git", "archive", REFERENCE_SHA, "app", "tests"],
                             cwd=ROOT, check=True, capture_output=True).stdout
    with tempfile.TemporaryDirectory(prefix="sub2ops-fallback-oracle-") as directory:
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
    path = ROOT / "tests/contracts/fallback/fixtures.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(fixture, ensure_ascii=False, sort_keys=True, indent=2) + "\n")
    print(json.dumps({"cases": len(fixture["cases"]), "reference_sha": REFERENCE_SHA}))


if __name__ == "__main__":
    main()
