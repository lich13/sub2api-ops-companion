#!/usr/bin/env python3
"""Compare operation request normalization with the pinned Python service."""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
import sys
import tempfile

from server_config_catalog_fixtures import export_pinned_app

ROOT = Path(__file__).resolve().parents[1]
REFERENCE_SHA = "6be7c291ed38dcc337cb88f693639040a3463899"
OUTPUT = ROOT / "tests/contracts/request-fixtures.json"


def cases() -> list[dict]:
    baselines = {
        "priority": {"priority": 1},
        "concurrency": {"concurrency": 1},
        "load_factor": {"load_factor": None},
        "proxy": {"proxy_id": None},
        "sync_models": {},
        "recover": {},
        "test": {"confirmed": True},
        "delete": {},
        "groups": {"scope_group_ids": [1], "group_ids": []},
        "schedulable": {"schedulable": False},
        "degradation_mark": {"marked": False},
        "usage": {"action": "query_usage"},
        "reset_quota": {"action": "reset_quota", "confirmed": True},
        "model_config": {},
        "account_template": {"template_id": "full", "template_version": "a" * 64},
    }
    variants = {
        "priority": {"priority": [None, True, False, -1, 0, 1.0, "1", {}, [], 2147483647, 2147483648]},
        "concurrency": {"concurrency": [None, True, False, -1, 0, 1.0, "1", {}, [], 2147483647, 2147483648]},
        "load_factor": {"load_factor": [True, False, -1, 0, 1, 1.0, "1", {}, [], 10000, 10001]},
        "proxy": {"proxy_id": [True, False, -1, 0, 1, 1.0, "1", {}, [], 9223372036854775807]},
        "test": {
            "confirmed": [None, False, 0, 1, "true", {}],
            "model_id": [None, 1, "", "模型", "x" * 200, "x" * 201],
            "mode": [None, "", "unknown", "default", "compact", "text", "image", "video", "search", "tts", "stt", "realtime"],
            "prompt": [None, 1, "", "x" * 16000, "x" * 16001],
            "image_data_url": [None, 1, "", "data:image/png;base64,AA=="],
            "audio_data_url": [None, 1, "", "data:audio/wav;base64,AA=="],
        },
        "delete": {"detach_managed": [None, True, False, 0, 1, "false"]},
        "schedulable": {
            "schedulable": [None, True, False, 0, 1, "false"],
            "detach_managed": [None, True, False, 0, 1, "false"],
        },
        "degradation_mark": {"marked": [None, True, False, 0, 1, "false"]},
        "groups": {
            "scope_group_ids": [None, [], [0], [-1], [1.0], [True], ["1"], list(range(100)), list(range(101))],
            "group_ids": [None, [], [0], [-1], [1.0], [True], ["1"], list(range(100)), list(range(101))],
        },
        "usage": {"action": [None, "", "query_usage", "query_reset_credits", "probe_quota", "reset_quota"], "confirmed": [None, True, False, 0, 1, "true"]},
        "reset_quota": {"action": [None, "", "query_usage", "query_reset_credits", "probe_quota", "reset_quota"], "confirmed": [None, True, False, 0, 1, "true"]},
        "account_template": {
            "template_id": [None, "", "full", "degraded", "takeover", "custom-" + "a" * 24, "custom-" + "g" * 24, "custom-" + "a" * 25],
            "template_version": [None, "", "a" * 63, "a" * 65, "模型" * 64],
        },
        "model_config": {
            "whitelist": [None, "model", [], [1], ["model-a"], ["model-a", "model-a"], ["model-*"], ["model a"], ["model-" + str(i) for i in range(300)], ["model-" + str(i) for i in range(301)]],
            "mappings": [None, {}, [], [{"source": "model-*", "target": "model-a"}], [{"source": "*", "target": "model-a"}], [{"source": "model-a", "target": "model-a"}], [{"source": "model-a", "target": "model-b", "extra": True}], [{"source": "model-a"}], [{"source": "model-*", "target": "model-*"}]],
        },
    }
    result = []

    def add(action: str, name: str, payload: dict) -> None:
        result.append({"id": f"{action}/{name}", "action": action, "payload": payload})

    for action, baseline in baselines.items():
        add(action, "default", copy.deepcopy(baseline))
        for field in baseline:
            payload = copy.deepcopy(baseline)
            del payload[field]
            add(action, "missing-" + field, payload)
        for field in ["unexpected", "expected_version", "expected_mark_version"]:
            payload = copy.deepcopy(baseline)
            payload[field] = {"ignored": "only-the-current-version-field"}
            add(action, "extra-" + field, payload)
        for field, values in variants.get(action, {}).items():
            for index, value in enumerate(values):
                payload = copy.deepcopy(baseline)
                payload[field] = value
                add(action, f"{field}-{index}", payload)
    return result


def build(reference: Path) -> dict:
    sys.path.insert(0, str(reference))
    from app.account_operations import AccountOperations
    from app.account_templates import TemplateBatchAccount
    from fastapi import HTTPException
    from pydantic import ValidationError
    from app.account_operations import OperationRequest, TestBatchRequest, ProxyBatchRequest, AccountBatchRequest
    from app.account_templates import TemplatesRequest, CustomTemplateRequest, TemplateBatchRequest
    from app.desktop_actions import PriorityRequest, GroupsRequest, DegradationMarkRequest, TestRequest
    from app.desktop_api import ScheduleRequest, AccountVersionRequest, DeleteRequest, UsageActionRequest, ConfigRequest, RetryRequest
    from app.model_tests import ModelTestRequest
    from app.model_detection import DetectionRequest
    from app.model_api import ResolveModel, SaveModel, ModelVersion

    result = cases()
    for item in result:
        try:
            normalized = AccountOperations.validate_payload(item["action"], item["payload"])
            item["expected"] = {"status": 200, "payload": normalized}
        except (ValidationError, HTTPException) as error:
            item["expected"] = {"status": getattr(error, "status_code", 422)}
    integer_cases = []
    for index, value in enumerate([
        None, {}, [], True, False, -1, 0, 1, 1.0, 1.5, 42, 42.0,
        "1", "+1", "01", "1.0", "1.00", "1_000", "1__0", "_1", "1_",
        " 1 ", "1e0", "1.", "001.000", ".0", "１２", "\t1\n", "--1", "+-1",
        "", "nan", "Infinity", "1.01", "1.0_0", "1_0.00", 9223372036854775807,
        "9223372036854775807", "9223372036854775807.00",
    ]):
        item = {"id": f"template-integer-{index:02}", "input": value}
        try:
            item["expected"] = {"status": 200, "value": TemplateBatchAccount(
                account_id=value, expected_version="a" * 64).account_id}
        except ValidationError:
            item["expected"] = {"status": 422}
        integer_cases.append(item)
    version = "a" * 64
    account = "/accounts/989996"
    batch_account = {"account_id": 989996, "expected_version": version}
    ids = {"request_id": "fixture-schema-request-040", "client_id": "fixture-schema-client-040"}
    blueprints = [
        (PriorityRequest, "POST", account + "/priority", {"priority": 1, "expected_version": version}),
        (GroupsRequest, "PUT", account + "/groups", {"scope_group_ids": [1], "group_ids": [], "expected_version": version}),
        (DegradationMarkRequest, "PUT", account + "/degradation-mark", {"marked": False, "expected_mark_version": version}),
        (TestRequest, "POST", account + "/test", {"confirmed": True, "model_id": "fixture-model", "mode": "default", "prompt": "", "image_data_url": "", "audio_data_url": "", "expected_version": version}),
        (ScheduleRequest, "POST", account + "/schedulable", {"schedulable": False, "detach_managed": False, "expected_version": version}),
        (AccountVersionRequest, "POST", account + "/recover-state", {"expected_version": version}),
        (DeleteRequest, "DELETE", account, {"detach_managed": False, "expected_version": version}),
        (UsageActionRequest, "POST", account + "/usage-action", {"action": "query_usage", "confirmed": False, "expected_version": version}),
        (ModelTestRequest, "POST", account + "/model-tests", {"model_id": "fixture-model", "request_id": ids["request_id"], "concurrency": 1, "expected_operation_version": None, "expected_version": version}),
        (DetectionRequest, "PUT", account + "/model-detection", {"enabled": False, "interval_minutes": 15, "model_id": "fixture-model", "expected_version": version}),
        (OperationRequest, "POST", account + "/operations", {**ids, "action": "priority", "payload": {"priority": 1}, "expected_version": version}),
        (TestBatchRequest, "POST", "/account-operations/batch", {**ids, "action": "test", "payload": {"confirmed": True}, "accounts": [batch_account]}),
        (ProxyBatchRequest, "POST", "/account-operations/batch", {**ids, "action": "proxy", "payload": {"proxy_id": None}, "accounts": [batch_account]}),
        (AccountBatchRequest, "POST", "/account-operations/batch", {**ids, "action": "groups", "payload": {}, "accounts": [{**batch_account, "payload": {"scope_group_ids": [1], "group_ids": []}}]}),
        (TemplateBatchRequest, "POST", "/account-templates/apply", {"template_id": "full", "template_version": version, "request_id": "1" * 32, "client_id": "2" * 32, "accounts": [batch_account]}),
        (TemplatesRequest, "PUT", "/account-templates", {"expected_version": version, "full": {}, "degraded": {}, "takeover": {}}),
        (CustomTemplateRequest, "POST", "/account-templates", {"expected_version": version, "name": "fixture-template", "whitelist": [], "mappings": []}),
        (ConfigRequest, "PUT", "/config/oauth", {"expected_revision": version, "changes": {}}),
        (RetryRequest, "POST", "/model-tests/fixture-missing/retry", {"request_id": ids["request_id"]}),
        (ResolveModel, "POST", "/model-groups/989996/reasoning/resolve", {"model": "fixture-model", "efforts": None, "default_effort": ""}),
        (SaveModel, "PUT", "/model-groups/989996/reasoning", {"model": "fixture-model", "efforts": ["low"], "default_effort": "low", "confirm_allowlist": False, "expected_binding": version, "expected_version": version, "expected_revision": version}),
        (ModelVersion, "DELETE", "/model-groups/989996/reasoning", {"model": "fixture-model", "expected_version": version, "expected_revision": version}),
    ]
    invalid_http = []
    for model, method, path, base in blueprints:
        model.model_validate(base)
        variants = [("unknown", {**copy.deepcopy(base), "fixture_private_input": "fixture-private-input"})]
        for name in base:
            missing = copy.deepcopy(base)
            del missing[name]
            variants.append((name + "-missing", missing))
            for index, value in enumerate([None, True, False, 0, 1, 1.0, {}, [], "", "fixture-private-input", ["1"], [True], [1.0], "a" * 65]):
                payload = copy.deepcopy(base)
                payload[name] = value
                variants.append((f"{name}-{index:02}", payload))
        for name in ("accounts", "full", "degraded", "takeover", "payload"):
            if name not in base:
                continue
            nested = base[name][0] if name == "accounts" else base[name]
            if not isinstance(nested, dict):
                continue
            nested_variants = [("unknown", {**copy.deepcopy(nested), "unexpected": True})]
            for field in nested:
                missing = copy.deepcopy(nested)
                del missing[field]
                nested_variants.append((field + "-missing", missing))
                for index, value in enumerate([None, True, False, 0, -1, 1.0, {}, [], "", "1", "a" * 63, "a" * 65, "g" * 64]):
                    item = copy.deepcopy(nested)
                    item[field] = value
                    nested_variants.append((f"{field}-{index:02}", item))
            for label, value in nested_variants:
                payload = copy.deepcopy(base)
                payload[name] = [value] if name == "accounts" else value
                variants.append((f"nested-{name}-{label}", payload))
        for label, payload in variants:
            try:
                model.model_validate(payload)
            except ValidationError:
                invalid_http.append({"id": f"{model.__name__}/{label}", "method": method, "path": path, "payload": payload, "expected_status": 422})
    return {"reference_sha": REFERENCE_SHA, "cases": result, "template_integer_cases": integer_cases, "invalid_http_cases": invalid_http}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="sub2ops-request-reference-") as directory:
        reference = Path(directory)
        export_pinned_app(reference, ROOT)
        expected = build(reference)
    text = json.dumps(expected, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if args.write:
        OUTPUT.write_text(text)
    elif not OUTPUT.is_file() or OUTPUT.read_text() != text:
        print("request fixtures differ from the pinned reference", file=sys.stderr)
        return 1
    print(f"request validation: {len(expected['cases'])} payload, {len(expected['template_integer_cases'])} template integer, {len(expected['invalid_http_cases'])} HTTP cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
