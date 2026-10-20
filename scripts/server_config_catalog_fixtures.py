#!/usr/bin/env python3
"""Generate Rust config/catalog contract fixtures from pinned Python sources."""

from __future__ import annotations

import copy
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from typing import Any

from fastapi import HTTPException


from server_reference import REFERENCE_SHA
CUSTOM_ID = "0123456789abcdef01234567"


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
    if isinstance(value, dict):
        return {str(key): jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(item) for item in value]
    return value


def account_template_view(data: dict[str, Any], digest: Any) -> dict[str, Any]:
    return {
        "configured": data["configured"],
        "version": digest(data),
        "templates": data["templates"],
        "template_meta": data.get("template_meta", {}),
        "template_order": data.get("template_order", ["full", "degraded", "takeover"]),
        "template_versions": {key: digest(value) for key, value in data["templates"].items()},
        "custom_templates": data.get("custom_templates", []),
    }


def build_fixtures(repo: Path, app_root: Path, work: Path) -> dict[str, Any]:
    sys.path.insert(0, str(app_root))

    from app import account_templates as account_templates_module
    from app.account_templates import AccountTemplates, CustomTemplateRequest
    from app.key_fallback import KeyFallbackController, KeyFallbackConfigError, parse_managed_account_ids
    from app.model_catalog import import_fields
    from app.model_config_store import ModelConfigStore
    from app.model_rules import revision, transform, validate_fields, validate_overrides
    from app.settings import load_settings

    templates_root = work / "templates"
    templates_root.mkdir()
    service = SimpleNamespace(
        r=SimpleNamespace(
            db=SimpleNamespace(),
            settings=SimpleNamespace(
                usage_query_state_path=str(templates_root / "usage-query-state.json"),
                audit_path=str(templates_root / "audit.jsonl"),
            ),
        )
    )

    migration_input = {
        "version": 1,
        "revision": 4,
        "configured": True,
        "templates": {
            "full": {"whitelist": ["fixture-full-model"], "mappings": []},
            "degraded": {
                "whitelist": ["fixture-base-model"],
                "mappings": [{"source": "fixture-input-*", "target": "fixture-base-model"}],
            },
            "takeover": {"whitelist": [], "mappings": []},
        },
    }
    migration_path = templates_root / "account-templates.json"
    migration_path.write_text(json.dumps(migration_input, ensure_ascii=False), encoding="utf-8")
    migrated = AccountTemplates(service)
    migration_view = migrated.view()
    migration_persisted = json.loads(migration_path.read_text(encoding="utf-8"))
    readonly_normalized = AccountTemplates._normalize(copy.deepcopy(migration_input))
    readonly_view = account_template_view(readonly_normalized, account_templates_module.digest)

    crud_root = work / "template-crud"
    crud_root.mkdir()
    crud_service = SimpleNamespace(
        r=SimpleNamespace(
            db=SimpleNamespace(),
            settings=SimpleNamespace(
                usage_query_state_path=str(crud_root / "usage-query-state.json"),
                audit_path=str(crud_root / "audit.jsonl"),
            ),
        )
    )
    crud = AccountTemplates(crud_service)
    initial_view = crud.view()
    create_data = {
        "name": "fixture custom",
        "whitelist": ["fixture-custom-model"],
        "mappings": [{"source": "fixture-custom-*", "target": "fixture-custom-model"}],
    }
    with patch.object(account_templates_module.secrets, "token_hex", return_value=CUSTOM_ID):
        created_view = crud.create_custom(
            CustomTemplateRequest(expected_version=initial_view["version"], **create_data)
        )
    desired = crud.desired(
        account_templates_module.TemplateApplication(
            expected_version="0" * 64,
            template_id=f"custom-{CUSTOM_ID}",
            template_version=created_view["template_versions"][f"custom-{CUSTOM_ID}"],
        )
    )
    update_data = {
        "name": "fixture custom edited",
        "whitelist": ["fixture-edited-model"],
        "mappings": [],
    }
    updated_view = crud.update_custom(
        f"custom-{CUSTOM_ID}",
        CustomTemplateRequest(expected_version=created_view["version"], **update_data),
    )
    deleted_view = crud.delete_custom(f"custom-{CUSTOM_ID}", updated_view["version"])

    profile_cases = [
        {
            "id": "valid_exact_and_suffix_mapping",
            "input": {
                "whitelist": ["fixture-exact"],
                "mappings": [{"source": "fixture-alias-*", "target": "fixture-exact"}],
            },
        },
        {
            "id": "mapping_wildcard_must_be_trailing",
            "input": {"whitelist": [], "mappings": [{"source": "fixture-*-tail", "target": "fixture-exact"}]},
        },
        {
            "id": "mapping_conflicts_with_whitelist",
            "input": {"whitelist": ["fixture-exact"], "mappings": [{"source": "fixture-exact", "target": "fixture-other"}]},
        },
        {
            "id": "unknown_request_field",
            "input": {"whitelist": [], "mappings": [], "future": True},
        },
    ]
    for case in profile_cases:
        try:
            parsed = account_templates_module.Template.model_validate(case["input"])
            case["accepted"] = True
            case["combined"] = account_templates_module.combine(parsed.model_dump())
        except (ValueError, HTTPException):
            case["accepted"] = False

    model_root = work / "model-config"
    model_root.mkdir()
    model_path = model_root / "model-config.json"
    model_state = {
        "schema": 1,
        "groups": {
            "17": {
                "overrides": {
                    "fixture-model": {
                        "display_name": "合成模型",
                        "context_window": 1024,
                        "max_context_window": 2048,
                        "future": {"enabled": False, "unset": None},
                    }
                },
                "reasoning": {
                    "fixture-model": {
                        "efforts": ["low", "high"],
                        "default_effort": "high",
                        "mode": "draft",
                    }
                },
                "updated_at": "2031-04-05T06:07:08+00:00",
            }
        },
    }
    model_path.write_text(json.dumps(model_state, ensure_ascii=False), encoding="utf-8")
    model_store = ModelConfigStore(str(model_path))
    model_bytes_before = model_path.read_bytes()
    model_group = model_store.group(17)
    model_read_unchanged = model_path.read_bytes() == model_bytes_before
    invalid_model_state = '{"schema":2,"groups":{"17":{"overrides":{}}}}'
    invalid_model_path = model_root / "invalid-model-config.json"
    invalid_model_path.write_text(invalid_model_state, encoding="utf-8")
    invalid_model_store = ModelConfigStore(str(invalid_model_path))
    try:
        invalid_model_store.read()
        invalid_model_rejected = False
    except ValueError:
        invalid_model_rejected = True
    invalid_model_unchanged = invalid_model_path.read_text(encoding="utf-8") == invalid_model_state

    revision_input = {
        "中文": "思考档位",
        "null": None,
        "whole": 4,
        "fraction": 1.25,
        "nested": {"z": False, "a": ["fixture", None]},
    }
    transform_input = {
        "body": {
            "models": [
                {
                    "slug": "fixture-model",
                    "display_name": "原生名称",
                    "model_messages": {"instructions_template": "native", "kept": True},
                },
                {"slug": "blocked-model", "display_name": "blocked"},
            ],
            "future_metadata": {"enabled": False},
        },
        "overrides": {
            "fixture-model": {
                "display_name": "配置名称优先",
                "model_messages": {"instructions_template": "configured", "added": False},
            },
            "fixture-alias": {"description": "合成别名描述"},
        },
        "allowlist": {"enabled": True, "models": ["fixture-*"]},
        "aliases": {
            "fixture-alias": {
                "slug": "ignored-source-slug",
                "display_name": "合成别名",
                "context_window": 4096,
                "input_modalities": ["text"],
                "model_messages": {"instructions_template": "alias"},
            }
        },
    }
    transform_expected = transform(
        transform_input["body"],
        transform_input["overrides"],
        transform_input["allowlist"],
        transform_input["aliases"],
    )

    field_cases = [
        {
            "id": "valid_nested_patch",
            "fields": {"display_name": "fixture", "model_messages": {"instructions_template": "fixture"}},
            "base": {"context_window": 1024, "max_context_window": 2048},
        },
        {"id": "identity_field_is_read_only", "fields": {"slug": "fixture-model"}, "base": {}},
        {
            "id": "boolean_field_rejects_integer",
            "fields": {"support_verbosity": 1},
            "base": {},
        },
        {
            "id": "effective_window_relationship",
            "fields": {"context_window": 4096},
            "base": {"max_context_window": 2048},
        },
    ]
    for case in field_cases:
        try:
            validate_fields(case["fields"], case["base"])
            case["accepted"] = True
        except ValueError:
            case["accepted"] = False
    override_cases = [
        {"id": "valid_exact_model", "input": {"fixture-model": {"display_name": "fixture"}}},
        {"id": "wildcard_model_id_rejected", "input": {"fixture-*": {"display_name": "fixture"}}},
        {"id": "invalid_field_type_rejected", "input": {"fixture-model": {"support_verbosity": 1}}},
    ]
    for case in override_cases:
        try:
            validate_overrides(case["input"])
            case["accepted"] = True
        except ValueError:
            case["accepted"] = False

    import_cases = [
        {
            "id": "explicit_reasoning_levels",
            "input": {
                "slug": "fixture-model",
                "supported_reasoning_levels": ["low", {"effort": "high", "description": "深度"}],
                "default_reasoning_level": "high",
                "reasoning_options": [{"type": "effort", "values": ["none"]}],
            },
        },
        {"id": "reasoning_disabled_synthesizes_none", "input": {"reasoning": False, "display_name": "ignore"}},
        {
            "id": "reasoning_options_deduplicated",
            "input": {
                "reasoning_options": [
                    {"type": "EFFORT", "values": ["low", "high", "low", 3]},
                    {"type": "budget", "values": ["max"]},
                    {"type": "effort", "values": ["max"]},
                ]
            },
        },
        {
            "id": "present_empty_levels_win_over_options",
            "input": {"supported_reasoning_levels": [], "reasoning_options": [{"type": "effort", "values": ["low"]}]},
        },
    ]
    for case in import_cases:
        case["expected"] = import_fields(case["input"])

    priority_root = work / "settings-priority"
    priority_root.mkdir()
    oauth_config = {
        "oauth_auto_reset_credit_enabled": False,
        "oauth_recovery_monitor_enabled": False,
        "oauth_daily_test_time": "07:30",
        "oauth_usage_refresh_concurrency": "5",
        "oauth_recovery_test_model_id": "fixture-file-model",
        "oauth_recovery_connection_account_ids": [11, 13],
        "oauth_recovery_model_account_ids": [17],
    }
    bark_config = {"enabled": False, "device_key": "", "server_url": "http://127.0.0.1:8090"}
    env = {
        "DATABASE_URL": "postgresql://example.invalid/fixture",
        "BASE_PATH": "/fixture-sub2ops",
        "OAUTH_AUTO_RESET_CREDIT_ENABLED": "true",
        "OAUTH_RECOVERY_MONITOR_ENABLED": "true",
        "OAUTH_DAILY_TEST_TIME": "08:15",
        "OAUTH_USAGE_REFRESH_CONCURRENCY": "9",
        "OAUTH_RECOVERY_TEST_CONCURRENCY": "7",
        "OAUTH_EARLY_PROBE_BATCH_SIZE": "19",
        "OAUTH_RECOVERY_TEST_MODEL_ID": "fixture-env-model",
    }
    oauth_path = priority_root / "oauth-config.json"
    bark_path = priority_root / "bark-config.json"
    oauth_path.write_text(json.dumps(oauth_config), encoding="utf-8")
    bark_path.write_text(json.dumps(bark_config), encoding="utf-8")
    env_for_settings = {
        **env,
        "OAUTH_CONFIG_PATH": str(oauth_path),
        "BARK_CONFIG_PATH": str(bark_path),
        "KEY_FALLBACK_CONFIG_PATH": str(priority_root / "key-fallback.json"),
        "KEEPALIVE_CONFIG_PATH": str(priority_root / "keepalive-state.json"),
    }
    with patch.dict(os.environ, env_for_settings, clear=True):
        settings = load_settings()
    priority_expected = {
        "oauth_auto_reset_credit_enabled": settings.oauth_auto_reset_credit_enabled,
        "oauth_recovery_monitor_enabled": settings.oauth_recovery_monitor_enabled,
        "oauth_daily_test_enabled": settings.oauth_daily_test_enabled,
        "oauth_daily_test_time": settings.oauth_daily_test_time,
        "oauth_usage_refresh_concurrency": settings.oauth_usage_refresh_concurrency,
        "oauth_recovery_test_concurrency": settings.oauth_recovery_test_concurrency,
        "oauth_early_probe_batch_size": settings.oauth_early_probe_batch_size,
        "oauth_recovery_test_model_id": settings.oauth_recovery_test_model_id,
        "oauth_recovery_connection_account_ids": settings.oauth_recovery_connection_account_ids,
        "oauth_recovery_model_account_ids": settings.oauth_recovery_model_account_ids,
    }

    fallback_root = work / "key-fallback"
    fallback_root.mkdir()
    fallback_path = fallback_root / "key-fallback.json"
    fallback_input = {
        "enabled": True,
        "managed_account_ids": [7, 7, 9],
        "coexist_account_ids": [7],
        "config_version": 3,
        "updated_at": "2031-04-05T06:07:08+00:00",
        "updated_by": "fixture-admin",
    }
    fallback_path.write_text(json.dumps(fallback_input), encoding="utf-8")
    controller = KeyFallbackController(
        SimpleNamespace(key_fallback_config_path=str(fallback_path)),
        SimpleNamespace(),
        base_url_provider=lambda: "http://127.0.0.1:8090",
        admin_token_provider=lambda: "",
    )
    parsed_fallback = controller.load_config()
    fallback_expected = {
        "enabled": parsed_fallback.enabled,
        "openai_enabled": parsed_fallback.openai_enabled,
        "grok_enabled": parsed_fallback.grok_enabled,
        "managed_account_ids": list(parsed_fallback.managed_account_ids),
        "coexist_account_ids": list(parsed_fallback.coexist_account_ids),
        "config_version": parsed_fallback.config_version,
        "updated_at": parsed_fallback.updated_at,
        "updated_by": parsed_fallback.updated_by,
        "valid": parsed_fallback.valid,
        "panel": controller.panel_snapshot(),
    }
    invalid_fallback = {"enabled": "fixture-invalid", "managed_account_ids": [7]}
    fallback_path.write_text(json.dumps(invalid_fallback), encoding="utf-8")
    invalid_parsed = controller.load_config()
    invalid_fallback_expected = {
        "valid": invalid_parsed.valid,
        "panel": controller.panel_snapshot(),
    }
    id_cases = [
        {"id": "numeric_ids_deduplicated", "input": [7, "9", 7], "accepted": True, "expected": [7, 9]},
        {"id": "zero_rejected", "input": [0], "accepted": False},
        {"id": "boolean_rejected", "input": [True], "accepted": False},
    ]
    for case in id_cases:
        try:
            case["expected"] = parse_managed_account_ids(case["input"])
            case["accepted"] = True
        except KeyFallbackConfigError:
            case["accepted"] = False

    return {
        "reference_sha": REFERENCE_SHA,
        "templates": {
            "migration": {
                "input": migration_input,
                "expected_view": migration_view,
                "expected_persisted": migration_persisted,
                "readonly_normalized": readonly_normalized,
                "readonly_view": readonly_view,
                "corrupt_json": "{fixture-invalid-json",
            },
            "crud": {
                "custom_id": f"custom-{CUSTOM_ID}",
                "create": create_data,
                "update": update_data,
                "initial_view": initial_view,
                "created_view": created_view,
                "updated_view": updated_view,
                "deleted_view": deleted_view,
                "desired_mapping": desired,
                "invalid_profiles": profile_cases,
            },
        },
        "model": {
            "revision": {"input": revision_input, "expected": revision(revision_input)},
            "transform": {**transform_input, "expected": transform_expected},
            "field_validation": field_cases,
            "override_validation": override_cases,
            "reasoning_import": import_cases,
            "model_config": {
                "state": model_state,
                "group_id": 17,
                "expected_group": model_group,
                "read_unchanged": model_read_unchanged,
                "corrupt_json": invalid_model_state,
                "corrupt_rejected": invalid_model_rejected,
                "corrupt_unchanged": invalid_model_unchanged,
            },
        },
        "settings_priority": {
            "oauth_config": oauth_config,
            "bark_config": bark_config,
            "env": env,
            "expected": jsonable(priority_expected),
            "rust_additions_0_1_48": {"model_detection_model_id": "gpt-6-luna"},
        },
        "key_fallback": {
            "legacy_input": fallback_input,
            "expected": jsonable(fallback_expected),
            "invalid_input": invalid_fallback,
            "invalid_expected": jsonable(invalid_fallback_expected),
            "managed_id_cases": id_cases,
        },
    }


def main() -> None:
    repo = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="sub2ops-pinned-config-catalog-") as temporary:
        scratch = Path(temporary)
        app_root = scratch / "source"
        export_pinned_app(app_root, repo)
        fixture_root = scratch / "fixtures"
        fixture_root.mkdir()
        fixtures = build_fixtures(repo, app_root, fixture_root)
    sys.stdout.write(json.dumps(fixtures, ensure_ascii=False, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
