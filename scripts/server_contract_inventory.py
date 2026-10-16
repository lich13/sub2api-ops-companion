#!/usr/bin/env python3
"""Build deterministic Python-server migration contracts from a pinned Git tree."""

from __future__ import annotations

import argparse
import ast
import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Any


from server_reference import REFERENCE_SHA
EXPECTED_TEST_FILES = 65
EXPECTED_TEST_FUNCTIONS = 884
REPO_ROOT = Path(__file__).resolve().parents[1]

OUTPUTS = {
    "migration-map": Path("tests/contracts/migration-map.json"),
    "version-vectors": Path("tests/contracts/version-vectors.json"),
}
MIGRATION_MAP_PATH = Path("tests/contracts/migration-map.json")
COVERAGE_MAP_PATH = Path("tests/contracts/coverage-map.json")
DEFAULT_UNCOVERED_REASON = (
    "尚无针对该旧测试全部关键断言的 Rust 测试映射；不能仅凭能力模块相似推定覆盖。"
)

CAPABILITIES = {
    "account_operations": {
        "name": "账户操作与配置",
        "description": "账户状态、设置、模板、质量检查及账户级批量操作。",
    },
    "automated_quota_reset": {
        "name": "自动额度重置",
        "description": "自动额度重置流程、边界及持久化行为。",
    },
    "credit_reset": {
        "name": "额度与重置观察",
        "description": "额度退化、手动重置、重置结果和额度快照。",
    },
    "alerting": {
        "name": "通知与容量告警",
        "description": "Bark 通知、网关告警和容量告警。",
    },
    "identity": {
        "name": "身份与客户端标识",
        "description": "Codex 身份字段及其兼容行为。",
    },
    "batch_operations": {
        "name": "连接与代理批处理",
        "description": "连接、原生账户及代理相关批量任务。",
    },
    "desktop_api": {
        "name": "桌面管理 API",
        "description": "桌面管理接口、操作、配置、错误及迁移行为。",
    },
    "model_testing": {
        "name": "模型测试任务",
        "description": "每日测活、测试任务和响应时延行为。",
    },
    "model_detection": {
        "name": "模型检测",
        "description": "检测计划、额度、适用性、重检和结果处置。",
    },
    "event_persistence": {
        "name": "事件与使用记录",
        "description": "事件持久化、PostgreSQL 记录和使用记录查询。",
    },
    "modeltrace_fingerprint": {
        "name": "ModelTrace 与指纹库",
        "description": "模型调用追踪、指纹库和指纹同步。",
    },
    "group_management": {
        "name": "分组管理",
        "description": "账户分组及相关管理行为。",
    },
    "public_http": {
        "name": "HTTP 集成与面板路由",
        "description": "HTTP 集成边界、面板路由和公开接口行为。",
    },
    "key_fallback": {
        "name": "Key 回退",
        "description": "Key 支持、选择与回退规则。",
    },
    "model_catalog": {
        "name": "模型目录与推理配置",
        "description": "模型目录变换、目录版本、推理配置及复合路由。",
    },
    "oauth_monitor": {
        "name": "OAuth 监控",
        "description": "OAuth 查询、监控及配置迁移。",
    },
    "proxy_diagnostics": {
        "name": "代理诊断",
        "description": "代理连通性、质量检查和原生 HTTP 行为。",
    },
    "recovery": {
        "name": "恢复流程",
        "description": "恢复选择、配置、派发、模型及参数恢复。",
    },
    "upstream_routing": {
        "name": "上游路由",
        "description": "上游路由规则和路由状态。",
    },
    "usage_query": {
        "name": "用量查询",
        "description": "用量查询协议和响应行为。",
    },
    "quota_snapshot": {
        "name": "额度快照",
        "description": "额度快照采集与展示行为。",
    },
}

MODULE_CAPABILITIES = {
    "test_account_batch031": "account_operations",
    "test_account_operations023": "account_operations",
    "test_account_quality": "account_operations",
    "test_account_settings035": "account_operations",
    "test_account_templates027": "account_operations",
    "test_auto_reset": "automated_quota_reset",
    "test_auto_reset037": "automated_quota_reset",
    "test_auto_reset037_boundaries": "automated_quota_reset",
    "test_auto_reset038": "automated_quota_reset",
    "test_bark": "alerting",
    "test_capacity_alerts": "alerting",
    "test_codex_identity": "identity",
    "test_connection_batch039": "batch_operations",
    "test_connection_batch_http039": "batch_operations",
    "test_controls022": "desktop_api",
    "test_credit_degradation037": "credit_reset",
    "test_credit_manual037": "credit_reset",
    "test_credit_results_http037": "credit_reset",
    "test_daily_test": "model_testing",
    "test_desktop_actions": "desktop_api",
    "test_desktop_api": "desktop_api",
    "test_desktop_errors": "desktop_api",
    "test_desktop_management": "desktop_api",
    "test_desktop_only_migration": "desktop_api",
    "test_desktop_usage": "desktop_api",
    "test_detection030": "model_detection",
    "test_detection_credits034": "model_detection",
    "test_detection_disposition027": "model_detection",
    "test_detection_rechecks034": "model_detection",
    "test_detection_schedulable037": "model_detection",
    "test_events030": "event_persistence",
    "test_fingerprint_bank": "modeltrace_fingerprint",
    "test_fingerprint_bank027": "modeltrace_fingerprint",
    "test_gateway_alerts022": "alerting",
    "test_group_management": "group_management",
    "test_http_integration030": "public_http",
    "test_key_fallback": "key_fallback",
    "test_key_support023": "key_fallback",
    "test_model_catalog": "model_catalog",
    "test_model_detection027": "model_detection",
    "test_model_reasoning": "model_catalog",
    "test_modeltrace019": "modeltrace_fingerprint",
    "test_modeltrace022": "modeltrace_fingerprint",
    "test_modeltrace023": "modeltrace_fingerprint",
    "test_modeltrace024": "modeltrace_fingerprint",
    "test_native_batch038": "batch_operations",
    "test_oauth_config_migration": "oauth_monitor",
    "test_oauth_monitor": "oauth_monitor",
    "test_oauth_queries": "oauth_monitor",
    "test_panel_routes": "public_http",
    "test_parameter_recovery035": "recovery",
    "test_postgres_events030": "event_persistence",
    "test_proxy_batch036": "batch_operations",
    "test_proxy_diagnostics037": "proxy_diagnostics",
    "test_proxy_native_http037": "proxy_diagnostics",
    "test_quota_snapshot": "quota_snapshot",
    "test_recovery019": "recovery",
    "test_recovery_config030": "recovery",
    "test_recovery_dispatch036": "recovery",
    "test_recovery_models030": "recovery",
    "test_recovery_selection034": "recovery",
    "test_slow_ttft026": "model_testing",
    "test_upstream_router": "upstream_routing",
    "test_usage_query": "usage_query",
    "test_usage_records": "event_persistence",
}


class InventoryError(RuntimeError):
    """Raised when the pinned source cannot be inventoried without ambiguity."""


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=REPO_ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    if result.returncode:
        raise InventoryError(
            f"git {' '.join(args)} failed ({result.returncode}): {result.stderr.strip()}"
        )
    return result.stdout


def git_tree_paths() -> list[str]:
    git("cat-file", "-e", f"{REFERENCE_SHA}^{{commit}}")
    return sorted(
        path for path in git("ls-tree", "-r", "--name-only", REFERENCE_SHA).splitlines()
    )


def git_show(path: str) -> str:
    return git("show", f"{REFERENCE_SHA}:{path}")


def source_module(path: str) -> str:
    return PurePosixPath(path).with_suffix("").as_posix().replace("/", ".")


def parse_test_functions(test_sources: dict[str, str]) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    for path, source in sorted(test_sources.items()):
        stem = PurePosixPath(path).stem
        capability_id = MODULE_CAPABILITIES.get(stem)
        if capability_id is None:
            raise InventoryError(f"test module has no service capability mapping: {path}")
        tree = ast.parse(source, filename=path)

        class TestVisitor(ast.NodeVisitor):
            def __init__(self) -> None:
                self.parents: list[str] = []
                self.function_depth = 0

            def visit_ClassDef(self, node: ast.ClassDef) -> None:
                self.parents.append(node.name)
                self.generic_visit(node)
                self.parents.pop()

            def _visit_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
                if node.name.startswith("test_"):
                    qualname = ".".join([*self.parents, node.name])
                    test_id = f"py:{source_module(path)}:{qualname}"
                    entries.append(
                        {
                            "id": test_id,
                            "source": {
                                "file": path,
                                "module": source_module(path),
                                "function": node.name,
                                "qualified_name": qualname,
                                "line": node.lineno,
                                "definition_scope": "nested" if self.function_depth else "module_or_class",
                                "pytest_collection": "nested_helper_not_collected" if self.function_depth else "candidate",
                            },
                            "service_capability": capability_id,
                            "rust_test": {
                                "status": "pending",
                                "crate": None,
                                "module": None,
                                "test_name": None,
                                "evidence": None,
                            },
                        }
                    )
                self.function_depth += 1
                self.parents.append(node.name)
                self.generic_visit(node)
                self.parents.pop()
                self.function_depth -= 1

            visit_FunctionDef = _visit_function
            visit_AsyncFunctionDef = _visit_function

        TestVisitor().visit(tree)

    entries.sort(key=lambda item: item["id"])
    ids = [entry["id"] for entry in entries]
    if len(ids) != len(set(ids)):
        duplicates = sorted(test_id for test_id in set(ids) if ids.count(test_id) > 1)
        raise InventoryError(f"duplicate stable test IDs: {duplicates[:10]}")
    actual_modules = {PurePosixPath(path).stem for path in test_sources}
    mapped_modules = set(MODULE_CAPABILITIES)
    if actual_modules != mapped_modules:
        raise InventoryError(
            "test module mapping mismatch: "
            f"unmapped={sorted(actual_modules - mapped_modules)}, "
            f"stale={sorted(mapped_modules - actual_modules)}"
        )
    if len(test_sources) != EXPECTED_TEST_FILES or len(entries) != EXPECTED_TEST_FUNCTIONS:
        raise InventoryError(
            f"pinned inventory count changed: {len(test_sources)} test files, "
            f"{len(entries)} test functions; expected {EXPECTED_TEST_FILES}/{EXPECTED_TEST_FUNCTIONS}"
        )
    return entries


def _normalize_source(value: str) -> str:
    collapsed = " ".join(value.split())
    return re.sub(r"\s*([()\[\]{},;])\s*", r"\1", collapsed)


def _skip_rust_non_code(source: str, index: int) -> int | None:
    """Return the end of a Rust comment/string/character literal at index."""
    length = len(source)
    if source.startswith("//", index):
        newline = source.find("\n", index + 2)
        return length if newline < 0 else newline + 1
    if source.startswith("/*", index):
        depth = 1
        cursor = index + 2
        while cursor < length and depth:
            if source.startswith("/*", cursor):
                depth += 1
                cursor += 2
            elif source.startswith("*/", cursor):
                depth -= 1
                cursor += 2
            else:
                cursor += 1
        return cursor

    raw_start = index
    if source.startswith("br", index):
        raw_start += 2
    elif source[index:index + 1] == "r":
        raw_start += 1
    else:
        raw_start = -1
    if raw_start >= 0:
        cursor = raw_start
        while cursor < length and source[cursor] == "#":
            cursor += 1
        if cursor < length and source[cursor] == '"':
            hashes = "#" * (cursor - raw_start)
            end = source.find('"' + hashes, cursor + 1)
            return length if end < 0 else end + 1 + len(hashes)

    if source[index:index + 1] == '"':
        cursor = index + 1
        while cursor < length:
            if source[cursor] == "\\":
                cursor += 2
            elif source[cursor] == '"':
                return cursor + 1
            else:
                cursor += 1
        return length

    if source[index:index + 1] == "'":
        # Lifetimes such as 'a have no closing quote; skip only actual chars.
        cursor = index + 1
        while cursor < length and source[cursor] != "\n":
            if source[cursor] == "\\":
                cursor += 2
            elif source[cursor] == "'":
                return cursor + 1
            else:
                cursor += 1
        return None
    return None


def _rust_test_body(path: str, test_name: str, *, annotated: bool = True) -> tuple[str, int]:
    if not path.startswith(("server/tests/", "server/src/")) or not path.endswith(".rs") or ".." in PurePosixPath(path).parts:
        raise InventoryError(f"Rust evidence path is outside the server crate: {path}")
    source_path = REPO_ROOT / path
    if not source_path.is_file():
        raise InventoryError(f"Rust evidence file is missing: {path}")
    source = source_path.read_text(encoding="utf-8")
    prefix = r"^\s*#\[(?:tokio::)?test[^\]]*\]\s*" if annotated else r"^\s*"
    pattern = re.compile(
        r"(?m)" + prefix + r"(?:async\s+)?fn\s+"
        + re.escape(test_name)
        + r"\s*\("
    )
    matches = list(pattern.finditer(source))
    if len(matches) != 1:
        raise InventoryError(
            f"expected one {'annotated test' if annotated else 'helper'} {path}::{test_name}, found {len(matches)}"
        )
    match = matches[0]
    body_start = source.find("{", match.end())
    if body_start < 0:
        raise InventoryError(f"Rust test has no body: {path}::{test_name}")
    depth = 0
    cursor = body_start
    while cursor < len(source):
        skipped = _skip_rust_non_code(source, cursor)
        if skipped is not None:
            cursor = skipped
            continue
        char = source[cursor]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                body = source[body_start:cursor + 1]
                line = source.count("\n", 0, match.start()) + 1
                return body, line
        cursor += 1
    raise InventoryError(f"unterminated Rust test body: {path}::{test_name}")


def _python_contract_body(
    path: str,
    suite_name: str,
    test_name: str,
    registration_method: str,
    case_name: str,
) -> tuple[str, int]:
    if not path.startswith("scripts/server_") or "contract" not in PurePosixPath(path).stem or not path.endswith(".py"):
        raise InventoryError(f"process-contract evidence path is outside scripts/server_*contract.py: {path}")
    source_path = REPO_ROOT / path
    if not source_path.is_file():
        raise InventoryError(f"process-contract evidence file is missing: {path}")
    source = source_path.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=path)
    suites = [node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == suite_name]
    if len(suites) != 1:
        raise InventoryError(f"expected one process-contract suite {path}::{suite_name}, found {len(suites)}")
    suite = suites[0]
    tests = [
        node for node in suite.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == test_name
    ]
    registrations = [
        node for node in suite.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == registration_method
    ]
    if len(tests) != 1 or len(registrations) != 1:
        raise InventoryError(
            f"expected one test and registration method in {path}::{suite_name}: {test_name}/{registration_method}"
        )
    registered = False
    for node in ast.walk(registrations[0]):
        if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
            continue
        if not isinstance(node.func.value, ast.Name) or node.func.value.id != "self" or node.func.attr != "case":
            continue
        if len(node.args) < 2 or _literal_string(node.args[0], {}) != case_name:
            continue
        callback = node.args[1]
        if (
            isinstance(callback, ast.Attribute)
            and isinstance(callback.value, ast.Name)
            and callback.value.id == "self"
            and callback.attr == test_name
        ):
            registered = True
            break
    if not registered:
        raise InventoryError(
            f"process-contract case {case_name!r} is not registered as {suite_name}.{test_name} in {path}"
        )
    body = ast.get_source_segment(source, tests[0])
    if body is None:
        raise InventoryError(f"could not extract process-contract method {path}::{suite_name}.{test_name}")
    return body, tests[0].lineno


def _fixture_value_at(document: Any, dotted_path: str) -> Any:
    value = document
    for part in dotted_path.split("."):
        if isinstance(value, dict) and part in value:
            value = value[part]
        elif isinstance(value, list) and part.isdigit() and int(part) < len(value):
            value = value[int(part)]
        else:
            raise InventoryError(f"fixture JSON path does not exist: {dotted_path}")
    return value


def _fixture_contains_named_id(value: Any, expected: str) -> bool:
    if isinstance(value, dict):
        if expected in value:
            return True
        if any(value.get(key) == expected for key in ("id", "name")):
            return True
        return any(_fixture_contains_named_id(item, expected) for item in value.values())
    if isinstance(value, list):
        return any(_fixture_contains_named_id(item, expected) for item in value)
    return False


def load_coverage_map(
    tests: list[dict[str, Any]],
    document: dict[str, Any] | None = None,
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    path = REPO_ROOT / COVERAGE_MAP_PATH
    if document is None:
        if not path.is_file():
            raise InventoryError(f"missing manually reviewed coverage map: {COVERAGE_MAP_PATH.as_posix()}")
        document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1 or document.get("reference_sha") != REFERENCE_SHA:
        raise InventoryError("coverage map schema or pinned Python reference SHA mismatch")

    by_id = {entry["id"]: entry for entry in tests}
    mappings: dict[str, dict[str, Any]] = {}
    for mapping in document.get("mappings", []):
        test_id = mapping.get("legacy_test_id")
        if test_id not in by_id:
            raise InventoryError(f"coverage mapping refers to unknown pinned test: {test_id}")
        if by_id[test_id]["source"]["pytest_collection"] != "candidate":
            raise InventoryError(f"coverage mapping refers to a non-collected helper: {test_id}")
        status = mapping.get("status")
        if status not in {"mapped", "partial", "superseded"}:
            raise InventoryError(f"unsupported static coverage status {status!r} for {test_id}")
        if test_id in mappings:
            raise InventoryError(f"duplicate coverage mapping for {test_id}")
        target = mapping.get("target")
        if not isinstance(target, dict):
            raise InventoryError(f"coverage mapping has no single Rust target: {test_id}")
        target_path = target.get("path")
        test_name = target.get("test_name")
        kind = target.get("kind", "rust_test")
        if kind == "rust_test":
            body, line = _rust_test_body(target_path, test_name)
            if target_path.startswith("server/src/"):
                verification_command = f".venv/bin/python scripts/server_test.py --lib --filter {test_name}"
            else:
                verification_command = f".venv/bin/python scripts/server_test.py --test {PurePosixPath(target_path).stem}"
        elif kind == "process_contract":
            body, line = _python_contract_body(
                target_path,
                target.get("suite"),
                test_name,
                target.get("registration_method"),
                target.get("case_name"),
            )
            verification_command = target.get("verification_command")
            if not isinstance(verification_command, str) or not verification_command.startswith(
                ".venv/bin/python scripts/server_"
            ):
                raise InventoryError(f"process-contract mapping needs its local runner command: {test_id}")
        else:
            raise InventoryError(f"unknown coverage target kind {kind!r} for {test_id}")
        anchors = target.get("assertions")
        if not anchors:
            raise InventoryError(f"coverage mapping has no assertion anchors: {test_id}")
        normalized_body = _normalize_source(body)
        for anchor in anchors:
            if not isinstance(anchor, str) or not anchor.strip():
                raise InventoryError(f"empty Rust assertion anchor in mapping for {test_id}")
            if _normalize_source(anchor) not in normalized_body:
                raise InventoryError(
                    f"assertion anchor missing from {target_path}::{test_name}: {anchor}"
                )

        helper_evidence = []
        reachable_source = body
        for helper in target.get("helper_assertions", []):
            name = helper.get("name")
            if kind != "rust_test" or not isinstance(name, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
                raise InventoryError(f"invalid Rust assertion helper for {test_id}")
            if not re.search(r"\b" + re.escape(name) + r"\s*\(", reachable_source):
                raise InventoryError(f"assertion helper {name} is not called by {test_name} or its declared helpers")
            helper_body, helper_line = _rust_test_body(target_path, name, annotated=False)
            helper_anchors = helper.get("assertions")
            if not helper_anchors or any(not isinstance(anchor, str) or not anchor.strip()
                                         or _normalize_source(anchor) not in _normalize_source(helper_body)
                                         for anchor in helper_anchors):
                raise InventoryError(f"assertion helper anchor missing from {target_path}::{name}")
            helper_evidence.append({"name": name, "line": helper_line, "assertions": helper_anchors})
            reachable_source += "\n" + helper_body

        fixture_evidence: list[dict[str, Any]] = []
        for fixture in target.get("fixtures", []):
            fixture_path = fixture.get("path")
            if not fixture_path.startswith("tests/contracts/") or not fixture_path.endswith(".json"):
                raise InventoryError(f"fixture path is outside tests/contracts JSON: {fixture_path}")
            fixture_file = REPO_ROOT / fixture_path
            if not fixture_file.is_file():
                raise InventoryError(f"Rust contract fixture is missing: {fixture_path}")
            fixture_document = json.loads(fixture_file.read_text(encoding="utf-8"))
            ids = fixture.get("ids", [])
            for fixture_id in ids:
                if not _fixture_contains_named_id(fixture_document, fixture_id):
                    raise InventoryError(f"fixture ID {fixture_id!r} not found in {fixture_path}")
            json_paths = fixture.get("json_paths", [])
            for json_path in json_paths:
                _fixture_value_at(fixture_document, json_path)
            fixture_evidence.append(
                {"path": fixture_path, "ids": ids, "json_paths": json_paths}
            )

        missing = mapping.get("uncovered_assertions", [])
        if status == "mapped" and missing:
            raise InventoryError(f"fully mapped test cannot list uncovered assertions: {test_id}")
        if status == "partial" and not missing:
            raise InventoryError(f"partial mapping must name its uncovered assertions: {test_id}")
        if status == "mapped" and not mapping.get("coverage_note"):
            raise InventoryError(f"full static mapping requires a coverage note: {test_id}")
        if status == "partial" and not mapping.get("coverage_note"):
            raise InventoryError(f"partial mapping requires a coverage note: {test_id}")
        replacement = mapping.get("replacement")
        if status == "superseded":
            if missing or not mapping.get("coverage_note") or not isinstance(replacement, dict):
                raise InventoryError(f"superseded mapping needs a complete replacement contract: {test_id}")
            for field in ("legacy_assertions", "approved_requirement", "replacement_contract"):
                if not replacement.get(field):
                    raise InventoryError(f"superseded mapping lacks {field}: {test_id}")
        elif replacement is not None:
            raise InventoryError(f"only superseded mappings may declare replacement semantics: {test_id}")

        mappings[test_id] = {
            "status": status,
            "target": {
                "path": target_path,
                "kind": kind,
                "module": PurePosixPath(target_path).stem,
                "test_name": test_name,
                "line": line,
                "assertions": anchors,
                "helper_assertions": helper_evidence,
                "fixtures": fixture_evidence,
                "verification_command": verification_command,
                "execution_status": "mapping_only",
            },
            "coverage_note": mapping["coverage_note"],
            "uncovered_assertions": missing,
            "replacement": replacement,
        }
        supporting = mapping.get("supporting_targets", [])
        if not isinstance(supporting, list):
            raise InventoryError(f"supporting targets must be an explicit list: {test_id}")
        if supporting:
            validated = []
            seen_targets = {(target_path, test_name)}
            for supporting_target in supporting:
                if not isinstance(supporting_target, dict):
                    raise InventoryError(f"supporting target must be an explicit object: {test_id}")
                identity = (supporting_target.get("path"), supporting_target.get("test_name"))
                if identity in seen_targets:
                    raise InventoryError(f"duplicate supporting target for {test_id}")
                seen_targets.add(identity)
                # A legacy test may span independent Rust cases. Validate every
                # registered case and assertion with the same strict rules;
                # merely naming a related module is still never coverage.
                nested = {key: value for key, value in mapping.items() if key != "supporting_targets"}
                nested["target"] = supporting_target
                nested_map, _ = load_coverage_map(tests, {
                    "schema_version": 1, "reference_sha": REFERENCE_SHA, "mappings": [nested],
                })
                validated.append(nested_map[test_id]["target"])
            mappings[test_id]["target"]["supporting_targets"] = validated

    uncovered_notes: dict[str, dict[str, Any]] = {}
    for note in document.get("uncovered_notes", []):
        test_id = note.get("legacy_test_id")
        if test_id not in by_id:
            raise InventoryError(f"uncovered note refers to unknown pinned test: {test_id}")
        if by_id[test_id]["source"]["pytest_collection"] != "candidate":
            raise InventoryError(f"uncovered note refers to a non-collected helper: {test_id}")
        if test_id in mappings or test_id in uncovered_notes:
            raise InventoryError(f"duplicate mapping or uncovered note for {test_id}")
        reason = note.get("reason")
        missing = note.get("missing_assertions")
        if not isinstance(reason, str) or not reason.strip() or not missing:
            raise InventoryError(f"uncovered note requires reason and specific missing assertions: {test_id}")
        uncovered_notes[test_id] = {"reason": reason, "missing_assertions": missing}
    return mappings, uncovered_notes


def apply_coverage_map(
    tests: list[dict[str, Any]],
    mappings: dict[str, dict[str, Any]],
    uncovered_notes: dict[str, dict[str, Any]],
) -> dict[str, int]:
    counts = {"mapped": 0, "partial": 0, "superseded": 0, "uncovered": 0, "not_collected": 0, "passed": 0}
    for entry in tests:
        test_id = entry["id"]
        source = entry["source"]
        coverage = mappings.get(test_id)
        if source["pytest_collection"] == "nested_helper_not_collected":
            entry["rust_test"] = {
                "status": "not_collected",
                "crate": None,
                "module": None,
                "test_name": None,
                "evidence": None,
                "gap": {"reason": "嵌套 test_* helper 不属于 pytest 收集项。", "uncovered_assertions": []},
            }
            counts["not_collected"] += 1
            continue
        if coverage is None:
            note = uncovered_notes.get(test_id)
            gap = note or {"reason": DEFAULT_UNCOVERED_REASON, "missing_assertions": []}
            entry["rust_test"] = {
                "status": "uncovered",
                "crate": None,
                "module": None,
                "test_name": None,
                "evidence": None,
                "gap": {
                    "reason": gap["reason"],
                    "uncovered_assertions": gap["missing_assertions"],
                },
            }
            counts["uncovered"] += 1
            continue
        status = coverage["status"]
        target = coverage["target"]
        entry["rust_test"] = {
            "status": status,
            "crate": "sub2ops-server",
            "module": target["module"],
            "test_name": target["test_name"],
            "evidence": {
                "mapping_file": COVERAGE_MAP_PATH.as_posix(),
                "python_reference_sha": REFERENCE_SHA,
                "rust_source": target["path"],
                "rust_source_line": target["line"],
                "assertions": target["assertions"],
                "helper_assertions": target["helper_assertions"],
                "fixtures": target["fixtures"],
                "verification_command": target["verification_command"],
                "execution_status": target["execution_status"],
                "coverage_note": coverage["coverage_note"],
            },
            "replacement": coverage["replacement"],
            "gap": {
                "reason": "该 Rust 测试只覆盖旧测试的部分断言。" if status == "partial" else None,
                "uncovered_assertions": coverage["uncovered_assertions"],
            } if status == "partial" else None,
        }
        if target.get("supporting_targets"):
            entry["rust_test"]["evidence"]["supporting_targets"] = target["supporting_targets"]
        counts[status] += 1
    if set(mappings) - {entry["id"] for entry in tests}:
        raise InventoryError("coverage map contains test IDs not in the pinned inventory")
    return counts


def _literal_string(node: ast.AST | None, constants: dict[str, str]) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name):
        return constants.get(node.id)
    return None


def _keyword(node: ast.Call, name: str) -> ast.AST | None:
    for keyword in node.keywords:
        if keyword.arg == name:
            return keyword.value
    return None


def _route_methods(attribute: str, call: ast.Call) -> list[str] | None:
    direct = {
        "get": "GET",
        "post": "POST",
        "put": "PUT",
        "patch": "PATCH",
        "delete": "DELETE",
        "options": "OPTIONS",
        "head": "HEAD",
        "trace": "TRACE",
    }
    if attribute in direct:
        return [direct[attribute]]
    if attribute in {"websocket", "websocket_route"}:
        return ["WEBSOCKET"]
    if attribute in {"route", "api_route"}:
        methods = _keyword(call, "methods")
        if methods is None:
            return ["GET"]
        if not isinstance(methods, (ast.List, ast.Tuple, ast.Set)):
            return None
        values = [item.value for item in methods.elts if isinstance(item, ast.Constant)]
        if len(values) != len(methods.elts) or any(not isinstance(value, str) for value in values):
            return None
        return sorted({value.upper() for value in values})
    return None


def _route_path(call: ast.Call, constants: dict[str, str]) -> str | None:
    node = call.args[0] if call.args else _keyword(call, "path")
    path = _literal_string(node, constants)
    if path is None:
        return None
    return path if path.startswith("/") else "/" + path


def _join_prefix(prefix: str, path: str) -> str:
    if not prefix:
        return path
    if not prefix.startswith("/"):
        prefix = "/" + prefix
    return prefix.rstrip("/") + "/" + path.lstrip("/")


class RouteVisitor(ast.NodeVisitor):
    def __init__(self, path: str, tree: ast.Module, constants: dict[str, str]) -> None:
        self.path = path
        self.module = source_module(path)
        self.constants = constants
        self.parents: list[str] = []
        self.routes: list[dict[str, Any]] = []
        self.handler_nodes: dict[str, ast.FunctionDef | ast.AsyncFunctionDef] = {}
        self.router_prefixes: dict[str, str] = {}
        self.included_routers: dict[str, list[str]] = {}
        self._collect_routers(tree)

    def _collect_routers(self, tree: ast.Module) -> None:
        for node in ast.walk(tree):
            if not isinstance(node, (ast.Assign, ast.AnnAssign)):
                continue
            value = node.value
            if not isinstance(value, ast.Call) or not isinstance(value.func, ast.Name) or value.func.id != "APIRouter":
                continue
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            prefix = _literal_string(_keyword(value, "prefix"), self.constants) or ""
            for target in targets:
                if isinstance(target, ast.Name):
                    previous = self.router_prefixes.get(target.id)
                    if previous is not None and previous != prefix:
                        raise InventoryError(f"router variable has multiple prefixes in {self.path}: {target.id}")
                    self.router_prefixes[target.id] = prefix

        for node in ast.walk(tree):
            if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute) or node.func.attr != "include_router":
                continue
            if not node.args or not isinstance(node.args[0], ast.Name):
                raise InventoryError(f"non-static include_router call in {self.path}:{node.lineno}")
            router_name = node.args[0].id
            extra_prefix = _literal_string(_keyword(node, "prefix"), self.constants) or ""
            self.included_routers.setdefault(router_name, []).append(extra_prefix)

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        self.parents.append(node.name)
        self.generic_visit(node)
        self.parents.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._visit_route_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._visit_route_function(node)

    def _visit_route_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        qualified_name = ".".join([*self.parents, node.name])
        handler = f"{self.module}.{qualified_name}"
        for decorator in node.decorator_list:
            if not isinstance(decorator, ast.Call) or not isinstance(decorator.func, ast.Attribute):
                continue
            methods = _route_methods(decorator.func.attr, decorator)
            if methods is None:
                continue
            local_path = _route_path(decorator, self.constants)
            if local_path is None:
                raise InventoryError(f"non-static route path in {self.path}:{decorator.lineno}")
            receiver = decorator.func.value.id if isinstance(decorator.func.value, ast.Name) else ""
            if receiver in self.router_prefixes:
                include_prefixes = self.included_routers.get(receiver, [])
                if not include_prefixes:
                    continue
                prefixes = [self.router_prefixes[receiver] + extra for extra in include_prefixes]
            else:
                prefixes = [""]
            for prefix in prefixes:
                effective_path = _join_prefix(prefix, local_path)
                for method in methods:
                    self.routes.append(
                        {
                            "id": f"http:{method}:{effective_path}",
                            "method": method,
                            "path": effective_path,
                            "handler": handler,
                            "source": {
                                "file": self.path,
                                "module": self.module,
                                "function": node.name,
                                "qualified_name": qualified_name,
                                "line": decorator.lineno,
                            },
                            "compatibility_group_id": None,
                            "compatibility_role": None,
                        }
                    )
                    self.handler_nodes[handler] = node
        self.parents.append(node.name)
        self.generic_visit(node)
        self.parents.pop()


def _module_constants(tree: ast.Module) -> dict[str, str]:
    constants: dict[str, str] = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    constants[target.id] = node.value.value
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            if isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
                constants[node.target.id] = node.value.value
    return constants


def _active_route_modules(main_source: str) -> set[str]:
    tree = ast.parse(main_source, filename="app/main.py")
    installers: dict[str, str] = {}
    for node in ast.walk(tree):
        if not isinstance(node, ast.ImportFrom) or node.level != 1 or not node.module:
            continue
        for alias in node.names:
            if alias.name.startswith("install_"):
                installers[alias.asname or alias.name] = node.module
    active = {"app.main"}
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if isinstance(node.func, ast.Name):
            name = node.func.id
        elif isinstance(node.func, ast.Attribute):
            name = node.func.attr
        else:
            continue
        if name in installers:
            active.add("app." + installers[name])
    return active


def _has_request_path_branch(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    return any(
        isinstance(item, ast.Attribute)
        and item.attr == "path"
        and isinstance(item.value, ast.Attribute)
        and item.value.attr == "url"
        for statement in node.body
        for item in ast.walk(statement)
    )


def _compatibility_groups(
    routes: list[dict[str, Any]],
    handler_nodes: dict[str, ast.FunctionDef | ast.AsyncFunctionDef],
) -> list[dict[str, Any]]:
    by_handler: dict[str, list[dict[str, Any]]] = {}
    for route in routes:
        by_handler.setdefault(route["handler"], []).append(route)

    groups: list[dict[str, Any]] = []

    def route_ref(route: dict[str, Any]) -> dict[str, str]:
        return {"method": route["method"], "path": route["path"], "handler": route["handler"]}

    # Pair explicitly named legacy endpoints with their same-method base handler.
    explicit_legacy_path_pairs: set[tuple[str, str, str]] = set()
    for alias_route in routes:
        alias_handler = alias_route["handler"]
        suffix = "_legacy_alias"
        if not alias_handler.endswith(suffix):
            continue
        primary_handler = alias_handler[:-len(suffix)]
        primary_candidates = [
            route for route in routes
            if route["method"] == alias_route["method"] and route["handler"] == primary_handler
        ]
        if len(primary_candidates) != 1:
            raise InventoryError(
                f"legacy alias route has no unique same-method base handler: {alias_route['id']}"
            )
        primary_route = primary_candidates[0]
        group_id = "legacy-alias:" + primary_handler
        groups.append(
            {
                "id": group_id,
                "primary_handler": primary_handler,
                "alias_handler": alias_handler,
                "routes": [route_ref(primary_route), route_ref(alias_route)],
                "primary_path": primary_route["path"],
                "compatibility_aliases": [
                    {"path": alias_route["path"], "methods": [alias_route["method"]]}
                ],
                "classification_basis": "alias endpoint name ends with _legacy_alias and pairs with a same-method base handler",
            }
        )
        primary_route["compatibility_group_id"] = group_id
        primary_route["compatibility_role"] = "primary"
        alias_route["compatibility_group_id"] = group_id
        alias_route["compatibility_role"] = "compatibility_alias"
        explicit_legacy_path_pairs.add(
            (alias_route["source"]["module"], primary_route["path"], alias_route["path"])
        )

    for handler, handler_routes in sorted(by_handler.items()):
        paths = sorted({route["path"] for route in handler_routes})
        if len(paths) < 2:
            continue
        function = handler_nodes[handler]
        path_literals = {
            item.value for statement in function.body for item in ast.walk(statement)
            if isinstance(item, ast.Constant) and isinstance(item.value, str) and item.value in paths
        }
        explicit_path_pairs = {
            (primary, alias)
            for module, primary, alias in explicit_legacy_path_pairs
            if module == handler_routes[0]["source"]["module"] and primary in paths and alias in paths
        }
        if _has_request_path_branch(function) and len(path_literals) == 1:
            primary_path = next(iter(path_literals))
            basis = "same handler; primary path is the unique declared path compared with request.url.path"
        elif len(explicit_path_pairs) == 1:
            primary_path = next(iter(explicit_path_pairs))[0]
            basis = "same handler exposes a path pair explicitly named as a legacy alias elsewhere in the module"
        else:
            primary_path = None
            basis = "same handler is declared for multiple paths; no primary path inferred"
        group_id = "handler:" + handler
        methods_by_path = {
            path: sorted({route["method"] for route in handler_routes if route["path"] == path})
            for path in paths
        }
        aliases = [
            {"path": path, "methods": methods_by_path[path]}
            for path in paths if path != primary_path
        ] if primary_path else []
        groups.append(
            {
                "id": group_id,
                "handler": handler,
                "same_handler_paths": [
                    {"path": path, "methods": methods_by_path[path]}
                    for path in paths
                ],
                "routes": [
                    {"path": path, "methods": methods_by_path[path]}
                    for path in paths
                ],
                "primary_path": primary_path,
                "compatibility_aliases": aliases,
                "classification_basis": basis,
            }
        )
        for route in handler_routes:
            route["compatibility_group_id"] = group_id
            if primary_path:
                route["compatibility_role"] = "primary" if route["path"] == primary_path else "compatibility_alias"
            else:
                route["compatibility_role"] = "same_handler_path"
    return groups


def parse_public_routes(app_sources: dict[str, str]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    active_modules = _active_route_modules(app_sources["app/main.py"])
    routes: list[dict[str, Any]] = []
    handler_nodes: dict[str, ast.FunctionDef | ast.AsyncFunctionDef] = {}
    declared_modules: set[str] = set()
    for path, source in sorted(app_sources.items()):
        tree = ast.parse(source, filename=path)
        visitor = RouteVisitor(path, tree, _module_constants(tree))
        visitor.visit(tree)
        if visitor.routes:
            declared_modules.add(source_module(path))
            if source_module(path) in active_modules:
                routes.extend(visitor.routes)
                handler_nodes.update(visitor.handler_nodes)
    inactive = declared_modules - active_modules
    if inactive:
        raise InventoryError(f"route declarations exist in modules not installed by app/main.py: {sorted(inactive)}")

    routes.sort(key=lambda route: (route["path"], route["method"], route["handler"]))
    identities = [(route["method"], route["path"]) for route in routes]
    if len(identities) != len(set(identities)):
        duplicates = sorted(identity for identity in set(identities) if identities.count(identity) > 1)
        raise InventoryError(f"duplicate public route method/path pairs: {duplicates}")
    for route in routes:
        route["compatibility_group_id"] = None
        route["compatibility_role"] = None
    groups = _compatibility_groups(routes, handler_nodes)
    if not routes:
        raise InventoryError("no public FastAPI routes were found")
    return routes, groups


def _find_function(tree: ast.Module, name: str) -> ast.FunctionDef | ast.AsyncFunctionDef:
    matches = [
        node for node in ast.walk(tree)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name
    ]
    if len(matches) != 1:
        raise InventoryError(f"expected one function named {name}, found {len(matches)}")
    return matches[0]


def _compile_function(
    source: str,
    path: str,
    name: str,
    namespace: dict[str, Any],
) -> Any:
    tree = ast.parse(source, filename=path)
    function = _find_function(tree, name)
    module = ast.Module(body=[function], type_ignores=[])
    ast.fix_missing_locations(module)
    exec(compile(module, path, "exec"), namespace)
    return namespace[name]


def _assigned_value_function(
    source: str,
    path: str,
    function_name: str,
    target_name: str,
    target_key: str,
    namespace: dict[str, Any],
) -> Any:
    function = _find_function(ast.parse(source, filename=path), function_name)
    expression: ast.expr | None = None
    for node in ast.walk(function):
        if not isinstance(node, ast.Assign):
            continue
        for target in node.targets:
            if (
                isinstance(target, ast.Subscript)
                and isinstance(target.value, ast.Name)
                and target.value.id == target_name
                and isinstance(target.slice, ast.Constant)
                and target.slice.value == target_key
            ):
                expression = node.value
                break
        if expression is not None:
            break
    if expression is None:
        raise InventoryError(f"assignment {target_name}[{target_key!r}] not found in {path}:{function_name}")
    generated = ast.FunctionDef(
        name="_inventory_value",
        args=ast.arguments(
            posonlyargs=[],
            args=[ast.arg(arg="row")],
            vararg=None,
            kwonlyargs=[],
            kw_defaults=[],
            kwarg=None,
            defaults=[],
        ),
        body=[ast.Return(value=expression)],
        decorator_list=[],
    )
    module = ast.Module(body=[generated], type_ignores=[])
    ast.fix_missing_locations(module)
    exec(compile(module, path, "exec"), namespace)
    return namespace["_inventory_value"]


def _json_safe(value: Any) -> Any:
    if isinstance(value, dt.datetime):
        return {
            "$type": "datetime",
            "value": value.isoformat(),
            "python_str": str(value),
        }
    if isinstance(value, dict):
        return {str(key): _json_safe(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_json_safe(item) for item in value]
    return value


def _datetime_marker(value: str) -> dict[str, str]:
    stamp = dt.datetime.fromisoformat(value)
    return {"$type": "datetime", "value": value, "python_str": str(stamp)}


def _load_hash_functions(sources: dict[str, str]) -> dict[str, Any]:
    operation_ns: dict[str, Any] = {}
    exec(compile(sources["app/operation_versions.py"], "app/operation_versions.py", "exec"), operation_ns)

    account_ns: dict[str, Any] = {
        "versions": operation_ns["versions"],
        "digest": operation_ns["digest"],
    }
    account_version = _compile_function(
        sources["app/account_templates.py"],
        "app/account_templates.py",
        "account_version",
        account_ns,
    )

    config_ns: dict[str, Any] = {"hashlib": hashlib, "json": json, "Any": Any}
    config_revision = _compile_function(
        sources["app/config_service.py"],
        "app/config_service.py",
        "revision",
        config_ns,
    )

    model_ns: dict[str, Any] = {}
    exec(compile(sources["app/model_rules.py"], "app/model_rules.py", "exec"), model_ns)

    desktop_ns: dict[str, Any] = {"hashlib": hashlib, "json": json}
    account_dto_version = _assigned_value_function(
        sources["app/desktop_api.py"],
        "app/desktop_api.py",
        "account_dto",
        "value",
        "version",
        desktop_ns,
    )

    catalog_ns: dict[str, Any] = {"revision": model_ns["revision"]}
    catalog_group_version = _assigned_value_function(
        sources["app/model_catalog.py"],
        "app/model_catalog.py",
        "group",
        "row",
        "version",
        catalog_ns,
    )

    return {
        "operation_digest": operation_ns["digest"],
        "operation_versions": operation_ns["versions"],
        "account_version": account_version,
        "config_revision": config_revision,
        "model_revision": model_ns["revision"],
        "account_dto_version": account_dto_version,
        "catalog_group_version": catalog_group_version,
    }


def _fixture_row(groups: list[str] | None = None) -> dict[str, Any]:
    return {
        "id": 31,
        "platform": "openai",
        "type": "oauth",
        "parent_account_id": None,
        "credential_version": "凭据版本-α",
        "proxy_id": 7,
        "group_ids": list(groups if groups is not None else ["组-β", "组-α"]),
        "status": "active",
        "error_message": None,
        "rate_limit_reset_at": dt.datetime(2031, 4, 5, 6, 7, 8, tzinfo=dt.timezone.utc),
        "overload_until": None,
        "temp_unschedulable_until": dt.datetime(2031, 4, 5, 6, 7, 8),
        "name": "合成账户-雪",
        "schedulable": True,
        "priority": 7,
        "concurrency": 4,
        "load_factor": 1.25,
        "extra": {
            "reset_credit_available": True,
            "parent_account_id": None,
            "openai_passthrough": False,
            "openai_oauth_passthrough": False,
        },
        "model_catalog_version": "目录版本-β",
        "model_mapping_version": 3,
        "model_mapping": {"gpt-6": "别名-六", "nullable": None},
        "credential_settings_version": 2,
        "model_sync_project": "项目-δ",
    }


def build_version_vectors(sources: dict[str, str]) -> dict[str, Any]:
    functions = _load_hash_functions(sources)
    vectors: list[dict[str, Any]] = []

    mixed = {
        "文字": "模型-雪",
        "null_value": None,
        "integer": 7,
        "fraction": 1.25,
        "timestamp": dt.datetime(2031, 4, 5, 6, 7, 8, tzinfo=dt.timezone.utc),
    }
    reversed_mixed = dict(reversed(list(mixed.items())))
    digest_expected = functions["operation_digest"](mixed)
    if functions["operation_digest"](reversed_mixed) != digest_expected:
        raise InventoryError("operation digest stopped sorting JSON object keys")
    vectors.append(
        {
            "id": "operation_digest_unicode_null_number_datetime",
            "source": "app/operation_versions.py#digest",
            "input": _json_safe(mixed),
            "expected_sha256": digest_expected,
            "assertions": ["Unicode is UTF-8 JSON text", "datetime uses Python str(value) via default=str"],
        }
    )

    account = _fixture_row()
    account_versions = functions["operation_versions"](account)
    vectors.append(
        {
            "id": "operation_versions_full_synthetic_account",
            "source": "app/operation_versions.py#versions",
            "input": _json_safe(account),
            "expected_versions": account_versions,
        }
    )
    model_config_cases = {
        "baseline": _fixture_row(),
        "mapping_version_incremented": _fixture_row(),
        "mapping_without_version": _fixture_row(),
        "mapping_without_version_changed": _fixture_row(),
        "catalog_observation_only_changed": _fixture_row(),
        "passthrough_changed": _fixture_row(),
        "credential_settings_version_changed": _fixture_row(),
    }
    model_config_cases["mapping_version_incremented"]["model_mapping_version"] = 4
    for key in ("mapping_without_version", "mapping_without_version_changed"):
        model_config_cases[key]["model_mapping_version"] = None
    model_config_cases["mapping_without_version_changed"]["model_mapping"] = {
        "gpt-6": "另一个合成别名",
        "nullable": None,
    }
    model_config_cases["catalog_observation_only_changed"]["model_catalog_version"] = "新目录观察版本"
    model_config_cases["passthrough_changed"]["extra"]["openai_oauth_passthrough"] = True
    model_config_cases["credential_settings_version_changed"]["credential_settings_version"] = 3
    model_config_versions = {
        name: functions["operation_versions"](row)["model_config"]
        for name, row in model_config_cases.items()
    }
    if model_config_versions["baseline"] == model_config_versions["mapping_version_incremented"]:
        raise InventoryError("model_mapping_version stopped invalidating model_config")
    if model_config_versions["mapping_without_version"] == model_config_versions["mapping_without_version_changed"]:
        raise InventoryError("model_mapping stopped affecting model_config without a model_mapping_version")
    if model_config_versions["baseline"] != model_config_versions["catalog_observation_only_changed"]:
        raise InventoryError("model_catalog_version unexpectedly affects model_config")
    if model_config_versions["baseline"] == model_config_versions["passthrough_changed"]:
        raise InventoryError("passthrough settings stopped invalidating model_config")
    if model_config_versions["baseline"] == model_config_versions["credential_settings_version_changed"]:
        raise InventoryError("credential_settings_version stopped invalidating model_config")
    vectors.append(
        {
            "id": "operation_versions_model_config_sensitivity",
            "source": "app/operation_versions.py#versions[model_config]",
            "cases": {name: _json_safe(row) for name, row in model_config_cases.items()},
            "expected_model_config_versions": model_config_versions,
            "assertions": [
                "model_mapping_version and credential_settings_version invalidate the version",
                "model mapping is hashed when model_mapping_version is absent",
                "passthrough settings invalidate the version",
                "model_catalog_version alone does not invalidate model_config",
            ],
        }
    )
    vectors.append(
        {
            "id": "account_template_version_from_operation_version",
            "source": "app/account_templates.py#account_version",
            "input": _json_safe(account),
            "expected_version": functions["account_version"](account),
        }
    )

    group_order_a = _fixture_row(["组-β", "组-α"])
    group_order_b = _fixture_row(["组-α", "组-β"])
    versions_a = functions["operation_versions"](group_order_a)
    versions_b = functions["operation_versions"](group_order_b)
    if versions_a != versions_b:
        raise InventoryError("operation_versions group order is no longer normalized")
    vectors.append(
        {
            "id": "operation_versions_group_order_invariance",
            "source": "app/operation_versions.py#versions",
            "cases": {
                "a": _json_safe(group_order_a),
                "b": _json_safe(group_order_b),
            },
            "expected_versions": {"a": versions_a, "b": versions_b},
            "assertions": ["all field versions are equal after group_ids permutation"],
        }
    )

    parent = _fixture_row(["组-α"])
    parent.update({"id": 17, "type": "oauth", "parent_account_id": None})
    direct_child = _fixture_row(["组-α"])
    direct_child.update({"id": 18, "type": "apikey", "parent_account_id": 17})
    direct_child["extra"]["parent_account_id"] = 17
    nested_child = _fixture_row(["组-α"])
    nested_child.update({"id": 18, "type": "apikey", "parent_account_id": None})
    nested_child["extra"]["parent_account_id"] = 17
    parent_versions = functions["operation_versions"](parent)
    direct_versions = functions["operation_versions"](direct_child)
    nested_versions = functions["operation_versions"](nested_child)
    vectors.append(
        {
            "id": "account_versions_parent_child_identity",
            "source": "app/operation_versions.py#versions; app/account_templates.py#account_version",
            "cases": {
                "parent": _json_safe(parent),
                "child_direct_parent_id": _json_safe(direct_child),
                "child_extra_parent_id": _json_safe(nested_child),
            },
            "expected": {
                "parent_account_template_version": functions["account_version"](parent),
                "child_direct_account_template_version": functions["account_version"](direct_child),
                "child_extra_account_template_version": functions["account_version"](nested_child),
                "parent_priority_version": parent_versions["priority"],
                "child_direct_priority_version": direct_versions["priority"],
                "child_extra_priority_version": nested_versions["priority"],
                "child_direct_model_config_version": direct_versions["model_config"],
                "child_extra_model_config_version": nested_versions["model_config"],
            },
            "assertions": [
                "direct and extra parent IDs produce equal stable scheduling versions",
                "direct parent_account_id remains part of operation identity and account template version",
            ],
        }
    )

    account_dto_row = {
        "id": 31,
        "platform": "openai",
        "type": "oauth",
        "schedulable": True,
        "updated_at": dt.datetime(2031, 4, 5, 6, 7, 8, tzinfo=dt.timezone.utc),
        "priority": 7,
        "group_ids": [9, 3],
    }
    account_dto_reversed_row = {**account_dto_row, "group_ids": [3, 9]}
    account_dto_expected = functions["account_dto_version"](account_dto_row)
    account_dto_reversed_expected = functions["account_dto_version"](account_dto_reversed_row)
    if account_dto_expected != account_dto_reversed_expected:
        raise InventoryError("desktop account DTO version stopped sorting group IDs")
    vectors.append(
        {
            "id": "desktop_account_dto_version_group_order_invariance",
            "source": "app/desktop_api.py#account_dto.value.version",
            "cases": {
                "group_ids_9_3": _json_safe(account_dto_row),
                "group_ids_3_9": _json_safe(account_dto_reversed_row),
            },
            "expected_versions": {
                "group_ids_9_3": account_dto_expected,
                "group_ids_3_9": account_dto_reversed_expected,
            },
            "assertions": ["group IDs are sorted before hashing", "datetime uses Python str(value) via default=str"],
        }
    )

    config_input = {
        "说明": "OAuth 配置-雪",
        "nullable": None,
        "count": 7,
        "ratio": 1.25,
        "updated_at": dt.datetime(2031, 4, 5, 6, 7, 8, tzinfo=dt.timezone.utc),
    }
    vectors.append(
        {
            "id": "config_revision_unicode_null_number_datetime",
            "source": "app/config_service.py#revision",
            "input": _json_safe(config_input),
            "expected_revision": functions["config_revision"](config_input),
            "assertions": ["sort_keys=true", "ensure_ascii uses Python default true", "datetime uses Python str(value)"],
        }
    )

    catalog_input_a = {
        "模型": {"name": "星云", "nullable": None, "weight": 1.25},
        "priority": 7,
    }
    catalog_input_b = {
        "priority": 7,
        "模型": {"weight": 1.25, "nullable": None, "name": "星云"},
    }
    catalog_expected = functions["model_revision"](catalog_input_a)
    if functions["model_revision"](catalog_input_b) != catalog_expected:
        raise InventoryError("model_rules revision stopped sorting JSON object keys")
    vectors.append(
        {
            "id": "model_rules_revision_compact_unicode_null_number",
            "source": "app/model_rules.py#revision",
            "input": _json_safe(catalog_input_a),
            "expected_revision": catalog_expected,
            "assertions": ["ensure_ascii=false", "sort_keys=true", "separators=(',', ':')", "allow_nan=false"],
        }
    )

    catalog_group = {
        "id": 17,
        "platform": "composite",
        "updated_at": dt.datetime(2031, 4, 5, 6, 7, 8, tzinfo=dt.timezone.utc),
        "model_allowlist": {"enabled": True, "models": ["gpt-*", "模型-*"]},
        "codex_models_manifest_config": {"enabled": False, "priority": 3},
    }
    vectors.append(
        {
            "id": "model_catalog_group_version",
            "source": "app/model_catalog.py#group.row.version; app/model_rules.py#revision",
            "input": _json_safe(catalog_group),
            "expected_version": functions["catalog_group_version"](catalog_group),
            "assertions": ["updated_at is converted to str before model_rules.revision"],
        }
    )

    catalog_account = {
        "id": 31,
        "platform": "openai",
        "type": "oauth",
        "credentials": {"model_mapping": {"gpt-*": "别名-六"}},
        "extra": {"openai_oauth_passthrough": False},
    }
    native_body = {
        "models": [
            {"slug": "gpt-6-astra", "display_name": "星云", "priority": 7, "nullable": None, "weight": 1.25}
        ],
        "future": {"保留": True},
    }
    final_body = {
        "models": [
            {"slug": "gpt-6-astra", "display_name": "自定义星云", "priority": 7, "nullable": None, "weight": 1.25}
        ],
        "future": {"保留": True},
    }
    catalog_routes = [
        {
            "id": 5,
            "enabled": True,
            "public_model": "模型-别名",
            "match_type": "exact",
            "endpoint": "responses",
            "target_platform": "openai",
            "upstream_model": "gpt-6-astra",
            "priority": 7,
        }
    ]
    vectors.extend(
        [
            {
                "id": "model_catalog_account_cache_identity",
                "source": "app/model_catalog.py#source_catalog identity; app/model_rules.py#revision",
                "input": _json_safe(catalog_account),
                "expected_revision": functions["model_revision"](catalog_account),
            },
            {
                "id": "model_catalog_native_body_revision",
                "source": "app/model_catalog.py#revision(original); app/model_rules.py#revision",
                "input": _json_safe(native_body),
                "expected_revision": functions["model_revision"](native_body),
            },
            {
                "id": "model_catalog_final_response_etag",
                "source": "app/model_catalog.py#revision(final); app/model_rules.py#revision",
                "input": _json_safe(final_body),
                "expected_etag_value": functions["model_revision"](final_body),
                "expected_etag_header": '"' + functions["model_revision"](final_body) + '"',
            },
            {
                "id": "model_catalog_accounts_routes_revision",
                "source": "app/model_catalog.py#revision([accounts, routes]); app/model_rules.py#revision",
                "input": _json_safe([[catalog_account], catalog_routes]),
                "expected_revision": functions["model_revision"]([[catalog_account], catalog_routes]),
            },
        ]
    )

    vectors.sort(key=lambda vector: vector["id"])
    vector_ids = [vector["id"] for vector in vectors]
    if len(vector_ids) != len(set(vector_ids)):
        raise InventoryError("duplicate version vector IDs")
    for vector in vectors:
        for key, value in vector.items():
            if key.startswith("expected_") and isinstance(value, str) and len(value.strip('"')) != 64:
                raise InventoryError(f"invalid SHA-256 value in vector {vector['id']}:{key}")

    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "purpose": "合成基线向量；供 Rust 端逐项复现 Python 版本、配置和模型目录哈希。",
        "typed_values": {
            "datetime": {
                "encoding": {"$type": "datetime", "value": "ISO-8601", "python_str": "Python str(datetime)"},
                "note": "仅脚本计算时解码为 datetime；Rust 对 default=str 的向量需产生 python_str 字段中的文本。",
            }
        },
        "canonical_json_strategies": [
            {
                "id": "operation_versions.digest",
                "source": "app/operation_versions.py#digest",
                "json_dumps": {"sort_keys": True, "ensure_ascii": False, "default": "str", "separators": "Python default"},
            },
            {
                "id": "config_service.revision",
                "source": "app/config_service.py#revision",
                "json_dumps": {"sort_keys": True, "ensure_ascii": True, "default": "str", "separators": "Python default"},
            },
            {
                "id": "desktop_api.account_dto.version",
                "source": "app/desktop_api.py#account_dto.value.version",
                "json_dumps": {"sort_keys": False, "ensure_ascii": True, "default": "str", "separators": "Python default", "input_shape": "ordered list"},
            },
            {
                "id": "model_rules.revision",
                "source": "app/model_rules.py#revision",
                "json_dumps": {"sort_keys": True, "ensure_ascii": False, "separators": [",", ":"], "allow_nan": False, "default": None},
            },
        ],
        "vectors": vectors,
    }


def build_migration_map(
    test_sources: dict[str, str],
    app_sources: dict[str, str],
) -> dict[str, Any]:
    tests = parse_test_functions(test_sources)
    mappings, uncovered_notes = load_coverage_map(tests)
    coverage_counts = apply_coverage_map(tests, mappings, uncovered_notes)
    routes, compatibility_groups = parse_public_routes(app_sources)
    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "source_scope": {
            "test_glob": "tests/test_*.py",
            "route_scope": "active FastAPI routes declared under app/ and installed from app/main.py",
            "test_files": len(test_sources),
            "test_functions": len(tests),
            "test_function_definitions": len(tests),
            "nested_helpers_not_collected": coverage_counts["not_collected"],
            "collected_test_candidates": len(tests) - coverage_counts["not_collected"],
            "public_route_method_paths": len(routes),
        },
        "rust_coverage_policy": {
            "status_semantics": (
                "mapped means source and assertion anchors were statically matched; it is not an execution pass. "
                "partial means the named target omits listed legacy assertions. superseded requires an "
                "explicit approved replacement requirement and an executable target that proves it; it does not "
                "claim the old behavior is preserved. uncovered means no exact target "
                "has been established. passed is reserved for an execution gate and is never inferred by this generator."
            ),
            "note": "映射依据为旧测试 ID、目标测试注册和断言锚点及显式 fixture；不得按模块相似推定覆盖。桌面测试、未关联的 Rust 测试及运行状态均不自动转成服务端覆盖或通过。",
            "coverage_counts": coverage_counts,
            "mapping_input": COVERAGE_MAP_PATH.as_posix(),
        },
        "service_capabilities": [
            {"id": capability_id, **CAPABILITIES[capability_id]}
            for capability_id in sorted(set(MODULE_CAPABILITIES.values()))
        ],
        "tests": tests,
        "routes": routes,
        "route_compatibility_groups": compatibility_groups,
    }


def render_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n"


def collect_inputs(tree_paths: list[str]) -> tuple[dict[str, str], dict[str, str], dict[str, str]]:
    test_paths = [path for path in tree_paths if path.startswith("tests/") and PurePosixPath(path).name.startswith("test_") and path.endswith(".py")]
    app_paths = [path for path in tree_paths if path.startswith("app/") and path.endswith(".py")]
    required_version_paths = {
        "app/operation_versions.py",
        "app/account_templates.py",
        "app/config_service.py",
        "app/model_rules.py",
        "app/desktop_api.py",
        "app/model_catalog.py",
    }
    missing = sorted(required_version_paths - set(app_paths))
    if missing or "app/main.py" not in app_paths:
        raise InventoryError(f"required pinned source files are missing: {missing or ['app/main.py']}")
    tests = {path: git_show(path) for path in test_paths}
    app = {path: git_show(path) for path in app_paths}
    version_sources = {path: app[path] for path in required_version_paths}
    return tests, app, version_sources


def write_outputs(documents: dict[str, Any], check_only: bool, names: list[str] | None = None) -> int:
    mismatches: list[str] = []
    for name in names or list(OUTPUTS):
        relative_path = OUTPUTS[name]
        path = REPO_ROOT / relative_path
        expected = render_json(documents[name])
        if check_only:
            if not path.is_file():
                mismatches.append(f"missing: {relative_path.as_posix()}")
                continue
            actual = path.read_text(encoding="utf-8")
            if actual != expected:
                mismatches.append(f"stale: {relative_path.as_posix()}")
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(expected, encoding="utf-8")
        print(f"wrote {relative_path.as_posix()}")

    if mismatches:
        for mismatch in mismatches:
            print(mismatch, file=sys.stderr)
        return 1
    if check_only:
        print("check passed: generated JSON files match the pinned source and reviewed coverage map")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify generated JSON without writing")
    parser.add_argument(
        "--require-complete", action="store_true",
        help="reject any collected Python test without a complete or approved replacement mapping",
    )
    parser.add_argument(
        "--write-migration-map-only",
        action="store_true",
        help="write only migration-map.json without touching other generated contract files",
    )
    args = parser.parse_args(argv)
    if args.check and args.write_migration_map_only:
        parser.error("--check cannot be combined with --write-migration-map-only")
    try:
        tree_paths = git_tree_paths()
        test_sources, app_sources, version_sources = collect_inputs(tree_paths)
        documents = {
            "migration-map": build_migration_map(test_sources, app_sources),
            "version-vectors": build_version_vectors(version_sources),
        }
        if args.require_complete:
            gaps = [entry["id"] for entry in documents["migration-map"]["tests"]
                    if entry["source"]["pytest_collection"] == "candidate"
                    and entry["rust_test"]["status"] not in {"mapped", "superseded"}]
            if gaps:
                raise InventoryError(
                    f"production replacement requires complete coverage; {len(gaps)} test mappings remain: "
                    + ", ".join(gaps[:8])
                )
        output_names = ["migration-map"] if args.write_migration_map_only else None
        return write_outputs(documents, args.check, output_names)
    except (InventoryError, OSError, SyntaxError, TypeError, ValueError) as exc:
        print(f"inventory error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
