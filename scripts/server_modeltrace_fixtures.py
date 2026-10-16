#!/usr/bin/env python3
"""Generate ModelTrace Rust contract fixtures from the pinned Python app tree."""
from __future__ import annotations

import copy
import json
import sys
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA

# Reuse the established isolated git-show exporter. The reference app is loaded
# only from a runner-owned temporary directory; no production config is read.
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts"))
from server_foundation_fixtures import export_pinned_app  # noqa: E402


def apply_mutation(value: Any, mutation: dict[str, Any]) -> Any:
    result = copy.deepcopy(value)
    operation = mutation["op"]
    if operation == "oversize_models":
        models = []
        for index in range(257):
            model = copy.deepcopy(result["models"][0])
            model["id"] = f"fixture-model-{index}"
            models.append(model)
        result["models"] = models
        result["robust"]["model_order"] = [model["id"] for model in models]
        return result

    path = mutation["path"]
    parent = result
    for component in path[:-1]:
        parent = parent[component]
    key = path[-1]
    if operation == "set":
        parent[key] = copy.deepcopy(mutation["value"])
    elif operation == "remove":
        del parent[key]
    elif operation == "truncate":
        parent[key] = parent[key][:-1]
    elif operation == "append":
        parent[key].append(copy.deepcopy(mutation["value"]))
    elif operation == "zero_vectors":
        parent[key] = [
            [0.0] * mutation["dimension"] for _ in range(mutation["count"])
        ]
    else:
        raise ValueError(f"unknown fixture mutation: {operation}")
    return result


def bank_validation_cases(bank: dict[str, Any], validate: Any) -> list[dict[str, Any]]:
    first_id = bank["models"][0]["id"]
    cases = [
        {"id": "valid_bundle", "mutation": None},
        {"id": "wrong_schema", "mutation": {"op": "set", "path": ["schema"], "value": "other"}},
        {"id": "missing_built_at", "mutation": {"op": "remove", "path": ["built_at"]}},
        {"id": "invalid_timestamp", "mutation": {"op": "set", "path": ["built_at"], "value": "not-a-timestamp"}},
        {"id": "naive_timestamp", "mutation": {"op": "set", "path": ["built_at"], "value": "2031-04-05T06:07:08"}},
        {"id": "minimum_count", "mutation": {"op": "set", "path": ["minimum_valid_numbers"], "value": 79}},
        {"id": "recommended_queries", "mutation": {"op": "set", "path": ["recommended_queries"], "value": 4}},
        {"id": "method_range", "mutation": {"op": "set", "path": ["method", "range"], "value": [1, 354]}},
        {"id": "method_alpha", "mutation": {"op": "set", "path": ["method", "alpha"], "value": 0.25}},
        {"id": "method_ordered_weight", "mutation": {"op": "set", "path": ["method", "ordered_block_weight"], "value": 0.5}},
        {"id": "empty_models", "mutation": {"op": "set", "path": ["models"], "value": []}},
        {"id": "too_many_models", "mutation": {"op": "oversize_models"}},
        {"id": "robust_not_ready", "mutation": {"op": "set", "path": ["robust", "robust_ready"], "value": False}},
        {"id": "duplicate_model_id", "mutation": {"op": "set", "path": ["models", 1, "id"], "value": first_id}},
        {"id": "missing_display_name", "mutation": {"op": "remove", "path": ["models", 0, "display_name"]}},
        {"id": "long_model_id", "mutation": {"op": "set", "path": ["models", 0, "id"], "value": "x" * 257}},
        {"id": "short_model_counts", "mutation": {"op": "truncate", "path": ["models", 0, "counts"]}},
        {"id": "fractional_count", "mutation": {"op": "set", "path": ["models", 0, "counts", 0], "value": 0.5}},
        {"id": "negative_count", "mutation": {"op": "set", "path": ["models", 0, "counts", 0], "value": -1}},
        {"id": "unsafe_integer_count", "mutation": {"op": "set", "path": ["models", 0, "counts", 0], "value": 9007199254740992}},
        {"id": "model_order", "mutation": {"op": "truncate", "path": ["robust", "model_order"]}},
        {"id": "short_hellinger_mean", "mutation": {"op": "truncate", "path": ["robust", "hellinger", "feature_mean"]}},
        {"id": "zero_hellinger_scale", "mutation": {"op": "set", "path": ["robust", "hellinger", "feature_scale", 0], "value": 0}},
        {"id": "too_many_hellinger_basis_rows", "mutation": {"op": "zero_vectors", "path": ["robust", "hellinger", "nuisance_basis"], "count": 356, "dimension": 355}},
        {"id": "short_ordered_basis_row", "mutation": {"op": "zero_vectors", "path": ["robust", "ordered_blocks", "nuisance_basis"], "count": 1, "dimension": 73}},
        {"id": "short_centroid_matrix", "mutation": {"op": "truncate", "path": ["robust", "hellinger", "centroids"]}},
        {"id": "wrong_ordered_weight", "mutation": {"op": "set", "path": ["robust", "ordered_blocks", "weight"], "value": 0}},
        {"id": "empty_environment_centroids", "mutation": {"op": "set", "path": ["robust", "ordered_blocks", "environment_centroids"], "value": []}},
        {"id": "short_environment_centroid", "mutation": {"op": "truncate", "path": ["robust", "ordered_blocks", "environment_centroids", 0, 0]}},
        {"id": "zero_calibration_beta", "mutation": {"op": "set", "path": ["calibration", "1", "beta"], "value": 0}},
        {"id": "out_of_range_cv_accuracy", "mutation": {"op": "set", "path": ["calibration", "3", "cv_accuracy"], "value": 1.01}},
    ]
    from app.fingerprint_bank import InvalidFingerprintBankError

    for case in cases:
        candidate = bank if case["mutation"] is None else apply_mutation(bank, case["mutation"])
        try:
            validate(candidate)
        except InvalidFingerprintBankError:
            case["expected_valid"] = False
        else:
            case["expected_valid"] = True
    return cases


def fixed_samples() -> list[dict[str, Any]]:
    state = 0x13579BDF
    random_values = []
    for _ in range(300):
        state ^= (state << 13) & 0xFFFFFFFF
        state ^= state >> 17
        state ^= (state << 5) & 0xFFFFFFFF
        state &= 0xFFFFFFFF
        random_values.append(state % 355 + 1)
    numbers = [
        list(range(1, 301)),
        [((index * 197 + 73) % 355) + 1 for index in range(300)],
        random_values,
    ]
    names = ["ascending_300", "affine_permutation_300", "fixed_xorshift_300"]
    return [
        {"id": name, "numbers": values, "text": ", ".join(map(str, values))}
        for name, values in zip(names, numbers, strict=True)
    ]


def validation_inputs() -> list[dict[str, Any]]:
    fence = chr(96) * 3
    newline = chr(10)
    return [
        {"id": "plain_range_filtering", "input": "+0001, 2  +003, 355, 356, -4, 0"},
        {"id": "mixed_unicode_separators_and_digits", "input": "[1，٢、३；4" + newline + "5]"},
        {"id": "accepted_text_fence", "input": fence + "json" + newline + "[1, 2, 3]" + newline + fence},
        {"id": "accepted_nested_thought", "input": "1,2<think>900<thinking>800</thinking>700</think>3"},
        {"id": "mismatched_thought_close", "input": "1<think>private</analysis>2"},
        {"id": "unclosed_thought", "input": "1,2<analysis>private 3"},
        {"id": "stray_thought_close", "input": "1,2</think>,3"},
        {"id": "unsupported_code_fence", "input": fence + "python" + newline + "1,2,3" + newline + fence},
        {"id": "unclosed_partial_tag", "input": "1,2<thi"},
        {"id": "unicode_bom", "input": chr(0xFEFF) * 2 + "[１, ١٢, १४, ๕]"},
        {"id": "non_decimal_unicode_number", "input": "1,²,3"},
        {"id": "invalid_decimal", "input": "1, 2.0, 3"},
        {"id": "invalid_nan_and_infinity", "input": "1, NaN, Infinity"},
        {"id": "empty_after_range_filter", "input": "-1,0,356,9999"},
        {"id": "parentheses_are_not_a_list", "input": "(1,2,3)"},
        {"id": "unclosed_fence", "input": fence + "text" + newline + "1,2,3"},
    ]


def isolated_inputs() -> list[dict[str, str]]:
    return [
        {"id": "simple_thought", "input": "answer<think>private 123</think> tail"},
        {"id": "nested_thought", "input": "A<thinking>B<analysis>C</analysis>D</thinking>E"},
        {"id": "mismatched_close", "input": "A<think>B</analysis>C</think>D"},
        {"id": "unclosed_thought", "input": "answer <reasoning>private 123"},
        {"id": "partial_tag_at_edge", "input": "answer 12 <thi"},
        {"id": "unicode_answer", "input": "答案١٢<think>秘密٣</think>４"},
    ]


def collector_cases() -> list[dict[str, Any]]:
    completed = {
        "status": "completed",
        "model": "fixture-model",
        "output": [
            {
                "type": "message",
                "phase": "final",
                "content": [{"type": "output_text", "text": "ModelTrace contract"}],
            }
        ],
        "usage": {
            "input_tokens": 23,
            "output_tokens": 19,
            "output_tokens_details": {"reasoning_tokens": 2},
        },
    }
    response_reasoning = {
        "status": "completed",
        "model": "fixture-model",
        "output": [
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "private 999"}]},
            {
                "type": "message",
                "phase": "final",
                "content": [{"type": "output_text", "text": "1,2,3"}],
            },
        ],
    }
    return [
        {
            "id": "responses_delta_then_snapshot",
            "expected_numbers": 3,
            "events": [
                {"event": {"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "phase": "final"}}},
                {"event": {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "ModelTrace "}},
                {"event": {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "contract"}},
                {"event": {"type": "response.output_text.done", "output_index": 0, "content_index": 0, "text": "ModelTrace contract"}},
                {"event": {"type": "response.completed", "response": completed}},
            ],
        },
        {
            "id": "reasoning_is_not_answer",
            "expected_numbers": 3,
            "events": [
                {"event": {"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "phase": "analysis"}}},
                {"event": {"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "private 999"}},
                {"event": {"type": "response.reasoning_summary_text.done", "output_index": 0, "summary_index": 0, "text": "private 999"}},
                {"event": {"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "phase": "final"}}},
                {"event": {"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "1,2,3"}},
                {"event": {"type": "response.completed", "response": response_reasoning}},
            ],
        },
        {
            "id": "whole_responses_json",
            "expected_numbers": 3,
            "events": [
                {
                    "whole": True,
                    "event": {
                        "object": "response",
                        "status": "completed",
                        "model": "fixture-model",
                        "output": [
                            {
                                "type": "message",
                                "phase": "final",
                                "content": [{"type": "output_text", "text": "whole response"}],
                            }
                        ],
                    },
                }
            ],
        },
        {
            "id": "chat_delta_and_reasoning",
            "expected_numbers": 3,
            "events": [
                {"event": {"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "answer ", "reasoning_content": "private"}, "finish_reason": None}]}},
                {"event": {"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "done"}, "finish_reason": None}]}},
                {"event": {"model": "fixture-model", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}},
            ],
        },
        {
            "id": "whole_chat_json",
            "expected_numbers": 3,
            "events": [
                {
                    "whole": True,
                    "event": {
                        "object": "chat.completion",
                        "model": "fixture-model",
                        "choices": [
                            {
                                "index": 0,
                                "message": {"content": "whole chat", "reasoning": "private"},
                                "finish_reason": "length",
                            }
                        ],
                    },
                }
            ],
        },
        {
            "id": "budget_incomplete_response",
            "expected_numbers": 3,
            "events": [
                {
                    "event": {
                        "type": "response.incomplete",
                        "response": {
                            "status": "incomplete",
                            "model": "fixture-model",
                            "incomplete_details": {"reason": "max_output_tokens"},
                            "output": [
                                {
                                    "type": "message",
                                    "phase": "final",
                                    "content": [{"type": "output_text", "text": "budget answer"}],
                                }
                            ],
                        },
                    }
                }
            ],
        },
        {
            "id": "unknown_incomplete_response",
            "expected_numbers": 3,
            "events": [
                {
                    "event": {
                        "type": "response.incomplete",
                        "response": {
                            "status": "incomplete",
                            "incomplete_details": {"reason": "unknown"},
                        },
                    }
                }
            ],
        },
        {
            "id": "invalid_protocol_index",
            "expected_numbers": 3,
            "events": [
                {"event": {"type": "response.output_text.delta", "output_index": "zero", "delta": "1"}},
            ],
        },
        {
            "id": "answer_number_guard",
            "expected_numbers": 1,
            "events": [
                {"event": {"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "1 2 "}, "finish_reason": None}]}},
                {"event": {"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "3 4 "}, "finish_reason": None}]}},
            ],
        },
    ]


def collector_projection(collector: Any, failure: Any = None) -> dict[str, Any]:
    return {
        "error": None if failure is None else {"code": failure.code, "retryable": failure.retryable},
        "text": collector.text,
        "completed": collector.completed,
        "returned_model": collector.returned_model,
        "recognized": collector.recognized,
        "protocol": collector.protocol,
        "end_reason": collector.end_reason,
        "metadata": collector.metadata,
    }


def collect_expected(case: dict[str, Any], Collector: Any, TestFailure: Any) -> dict[str, Any]:
    collector = Collector(case["expected_numbers"])
    failure = None
    for item in case["events"]:
        try:
            collector.accept(
                item["event"],
                item.get("name", ""),
                whole=item.get("whole", False),
            )
        except TestFailure as exc:
            failure = exc
            break
    return collector_projection(collector, failure)


def pkt_line(payload: bytes) -> bytes:
    return f"{len(payload) + 4:04x}".encode("ascii") + payload


def git_ref_inputs() -> list[dict[str, Any]]:
    oid = "0123456789abcdef0123456789abcdef01234567"
    newline = bytes([10])
    target = oid.encode() + b" refs/heads/hanmo" + bytes([0]) + b"multi_ack side-band-64k" + newline
    main = (("f" * 40) + " refs/heads/main").encode() + newline
    values = [
        ("valid_service_preamble", pkt_line(b"# service=git-upload-pack" + newline) + b"0000" + pkt_line(target) + pkt_line(main) + b"0000", "hanmo"),
        ("prefix_branch_is_not_exact", pkt_line((("a" * 40) + " refs/heads/hanmo-extra").encode() + newline) + b"0000", "hanmo"),
        ("duplicate_target", pkt_line(target) + pkt_line(target), "hanmo"),
        ("truncated_packet", b"0010short"),
        ("invalid_header", b"zzzz"),
        ("invalid_utf8", pkt_line(bytes([255, 254, 253, 10]))),
        ("missing_target", pkt_line(main) + b"0000"),
    ]
    output = []
    from app.fingerprint_bank import SyncFailure, parse_git_refs

    for item in values:
        name, raw, *branch_value = item
        branch = branch_value[0] if branch_value else "hanmo"
        try:
            result = {"revision": parse_git_refs(raw, branch)}
        except SyncFailure as exc:
            result = {"error_code": exc.code}
        output.append({"id": name, "branch": branch, "raw_hex": raw.hex(), "expected": result})
    return output


def target_identity_inputs(codex_identity: Any) -> list[dict[str, Any]]:
    inputs = [
        {
            "id": "manual_precedes_synced_and_versions_sync",
            "input": {
                "openai_codex_client_version": "0.150.1",
                "openai_codex_client_version_synced": "0.155.0",
                "openai_codex_user_agent": "CODEX_CLI_RS/0.1.0 (Ubuntu; x86_64)",
            },
        },
        {
            "id": "synced_version_after_invalid_manual",
            "input": {
                "openai_codex_client_version": "release-latest",
                "openai_codex_client_version_synced": "0.144.2-rc.1",
                "openai_codex_user_agent": "Codex-TUI/0.120.0 (Linux; x86_64)",
            },
        },
        {
            "id": "below_minimum_uses_builtin",
            "input": {
                "openai_codex_client_version": "0.143.99",
                "openai_codex_client_version_synced": "0.143.98",
                "openai_codex_user_agent": "codex_app/0.1.0 (macOS; arm64)",
            },
        },
        {
            "id": "four_components_and_prerelease",
            "input": {
                "openai_codex_client_version": "1.2.3.4-preview.5",
                "openai_codex_user_agent": "codex_vscode/1.0.0 (Windows; codex-tui/0.1.0)",
            },
        },
        {
            "id": "originator_from_trailer",
            "input": {
                "openai_codex_client_version": "0.146.7",
                "openai_codex_user_agent": "third_party/1.0.0 (codex-tui; Linux x86_64)",
            },
        },
        {
            "id": "invalid_unicode_user_agent",
            "input": {
                "openai_codex_client_version": "0.146.0",
                "openai_codex_user_agent": "codex-tui/0.1.0 (Linux; 台灣)",
            },
        },
        {
            "id": "oversized_user_agent",
            "input": {
                "openai_codex_client_version": "0.146.0",
                "openai_codex_user_agent": "codex-tui/" + ("x" * 520),
            },
        },
        {
            "id": "non_string_values",
            "input": {
                "openai_codex_client_version": 123,
                "openai_codex_client_version_synced": "0.146.1",
                "openai_codex_user_agent": False,
            },
        },
    ]
    for case in inputs:
        case["expected"] = list(codex_identity(case["input"]))
    return inputs


def target_signature_inputs(target_signature: Any) -> list[dict[str, Any]]:
    base = {
        "id": 17,
        "parent_account_id": 4,
        "platform": "openai",
        "type": "oauth",
        "credentials": {"fixture": "example.invalid", "mode": "oauth"},
        "proxy_id": 3,
        "extra": {
            "openai_responses_mode": "force_responses",
            "openai_responses_supported": True,
            "fixture_note": "synthetic",
        },
        "name": "fixture account",
    }
    rows = [
        ("full", base),
        ("untracked_name_change", {**base, "name": "renamed fixture"}),
        ("untracked_extra_change", {**base, "extra": {**base["extra"], "fixture_note": "changed"}}),
        ("credentials_change", {**base, "credentials": {"fixture": "example.invalid/changed", "mode": "oauth"}}),
        ("proxy_change", {**base, "proxy_id": 8}),
        ("protocol_change", {**base, "extra": {**base["extra"], "openai_responses_mode": "force_chat_completions"}}),
        ("sparse_row", {"id": 18, "platform": "openai", "type": "oauth"}),
        ("sparse_protocol", {"id": 19, "platform": "openai", "type": "apikey", "extra": {"openai_responses_mode": "force_chat_completions"}}),
    ]
    return [
        {"id": name, "row": row, "expected_signature": target_signature(row)}
        for name, row in rows
    ]


def stream_execute_cases() -> list[dict[str, Any]]:
    from app.model_test_stream import MAX_FRAME

    def json_bytes(value: Any) -> str:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))

    def sse(*events: dict[str, Any]) -> str:
        frames = []
        for event in events:
            name = event.get("type", "")
            frames.append("event: " + name + chr(10) + "data: " + json_bytes(event) + chr(10) + chr(10))
        return "".join(frames)

    responses = {
        "type": "response.completed",
        "response": {
            "id": "resp_fixture",
            "status": "completed",
            "model": "fixture-model",
            "output": [
                {
                    "type": "message",
                    "phase": "final",
                    "content": [{"type": "output_text", "text": "fixture answer"}],
                }
            ],
            "usage": {
                "input_tokens": 7,
                "output_tokens": 3,
                "output_tokens_details": {"reasoning_tokens": 1},
            },
        },
    }
    cases = [
        {
            "id": "responses_sse_delta_snapshot_terminal",
            "protocol": "responses",
            "body": sse(
                {"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "phase": "final"}},
                {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "fixture "},
                {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "answer"},
                responses,
            ),
            "status": 200,
            "content_type": "text/event-stream",
        },
        {
            "id": "chat_sse_done_terminal",
            "protocol": "chat_completions",
            "body": "data: " + json_bytes({"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "fixture answer"}, "finish_reason": None}]}) + chr(10) + chr(10)
            + "data: [DONE]" + chr(10) + chr(10),
            "status": 200,
            "content_type": "text/event-stream",
        },
        {
            "id": "responses_whole_json",
            "protocol": "responses",
            "body": json_bytes(responses["response"]),
            "status": 200,
            "content_type": "application/json",
        },
        {
            "id": "chat_whole_json",
            "protocol": "chat_completions",
            "body": json_bytes(
                {
                    "object": "chat.completion",
                    "model": "fixture-model",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"content": "fixture answer", "reasoning_content": "private"},
                            "finish_reason": "stop",
                        }
                    ],
                }
            ),
            "status": 200,
            "content_type": "application/json",
        },
        {
            "id": "responses_budget_terminal",
            "protocol": "responses",
            "body": sse(
                {
                    "type": "response.incomplete",
                    "response": {
                        "status": "incomplete",
                        "model": "fixture-model",
                        "incomplete_details": {"reason": "max_output_tokens"},
                        "output": [
                            {
                                "type": "message",
                                "phase": "final",
                                "content": [{"type": "output_text", "text": "budget answer"}],
                            }
                        ],
                    },
                }
            ),
            "status": 200,
            "content_type": "text/event-stream",
        },
        {
            "id": "missing_terminal",
            "protocol": "chat_completions",
            "body": "data: " + json_bytes({"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "partial"}, "finish_reason": None}]}) + chr(10) + chr(10),
            "status": 200,
            "content_type": "text/event-stream",
        },
        {
            "id": "truncated_sse_json",
            "protocol": "responses",
            "body": 'data: {"type":"response.output_text.delta","output_index":0,"delta":"partial"',
            "status": 200,
            "content_type": "text/event-stream",
        },
        {
            "id": "protocol_mismatch",
            "protocol": "responses",
            "body": json_bytes({"object": "unknown", "value": 1}),
            "status": 200,
            "content_type": "application/json",
        },
        {
            "id": "auth_http_error",
            "protocol": "responses",
            "body": json_bytes({"error": {"message": "invalid_api_key"}}),
            "status": 401,
            "content_type": "application/json",
        },
        {
            "id": "rate_limit_http_error",
            "protocol": "responses",
            "body": json_bytes({"error": {"message": "rate_limit_exceeded"}}),
            "status": 429,
            "content_type": "application/json",
        },
        {
            "id": "model_request_http_error",
            "protocol": "responses",
            "body": json_bytes({"error": {"message": "model_not_found"}}),
            "status": 400,
            "content_type": "application/json",
        },
        {
            "id": "upstream_http_error",
            "protocol": "responses",
            "body": json_bytes({"error": {"message": "fixture upstream failure"}}),
            "status": 502,
            "content_type": "application/json",
        },
        {
            "id": "disconnected_after_delta",
            "protocol": "chat_completions",
            "body": "data: " + json_bytes({"model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "partial"}, "finish_reason": None}]}) + chr(10) + chr(10),
            "status": 200,
            "content_type": "text/event-stream",
            "disconnect": True,
        },
        {
            "id": "frame_output_limit",
            "protocol": "responses",
            "body": "x" * (MAX_FRAME + 1),
            "status": 200,
            "content_type": "text/event-stream",
        },
    ]
    return cases


def execute_expected(case: dict[str, Any], execute: Any, TestFailure: Any) -> dict[str, Any]:
    import asyncio
    import httpx

    requests: list[dict[str, Any]] = []

    class BytesStream(httpx.AsyncByteStream):
        def __init__(self, body: bytes, disconnect: bool):
            self.body = body
            self.disconnect = disconnect

        async def __aiter__(self):
            yield self.body
            if self.disconnect:
                request = httpx.Request("POST", "http://example.invalid/model-test")
                raise httpx.ReadError("fixture disconnect", request=request)

        async def aclose(self) -> None:
            return None

    async def handler(request: Any) -> Any:
        requests.append(json.loads(request.content))
        return httpx.Response(
            case["status"],
            headers={"content-type": case["content_type"]},
            stream=BytesStream(case["body"].encode("utf-8"), case.get("disconnect", False)),
        )

    async def run() -> dict[str, Any]:
        diagnostics: dict[str, Any] = {}
        outcome: dict[str, Any]
        async with httpx.AsyncClient(transport=httpx.MockTransport(handler), trust_env=False) as client:
            try:
                text, model = await execute(
                    "http://example.invalid/model-test",
                    {},
                    None,
                    "fixture-model",
                    "synthetic prompt",
                    3,
                    oauth=False,
                    protocol=case["protocol"],
                    client=client,
                    diagnostics=diagnostics,
                )
                outcome = {"text": text, "model": model}
            except TestFailure as exc:
                outcome = {"error_code": exc.code, "retryable": exc.retryable}
            except httpx.HTTPError:
                outcome = {"error_code": diagnostics.get("end_reason", "network_error")}
        observed = {
            key: diagnostics.get(key)
            for key in ("protocol", "bytes", "events", "http_status", "response_protocol", "end_reason")
        }
        return {
            "request_count": len(requests),
            "request_models": [request.get("model") for request in requests],
            "request_payload": requests[0] if requests else None,
            "outcome": outcome,
            "diagnostics": observed,
        }

    return asyncio.run(run())


def failure_inputs(failure_fn: Any) -> list[dict[str, Any]]:
    values = [
        ("http_401", 401, {}),
        ("http_402", 402, {}),
        ("http_403", 403, {}),
        ("auth_text", 500, {"error": {"message": "invalid_api_key"}}),
        ("auth_precedes_rate", 429, {"error": {"message": "token_expired rate_limit"}}),
        ("http_429", 429, {}),
        ("usage_limit_text", 500, {"error": {"message": "usage_limit_reached"}}),
        ("concurrency_limit_text", 500, {"error": {"message": "concurrency_limit_exceeded"}}),
        ("http_400", 400, {}),
        ("model_not_found_text", 500, {"error": {"message": "model_not_found"}}),
        ("unknown_502", 502, {"error": {"message": "fixture gateway unavailable"}}),
    ]
    output = []
    from app.model_test_stream import TestFailure

    for name, status, payload in values:
        result = failure_fn(status, payload)
        if not isinstance(result, TestFailure):
            raise TypeError(f"unexpected old failure value for {name}")
        output.append({"id": name, "status": status, "payload": payload, "expected": {"code": result.code, "retryable": result.retryable}})
    return output


def build_fixtures(app_root: Path) -> dict[str, Any]:
    sys.path.insert(0, str(app_root))
    from app.codex_identity import codex_identity
    from app.fingerprint_bank import git_blob_digest, validate_fingerprint_bank
    from app.model_test_stream import Collector, TestFailure, execute, failure, isolated_answer
    from app.model_tests import target_signature
    from app.modeltrace import analyze, bundled_bank, challenges, scores, validate

    bank, bank_version = bundled_bank()
    validate_fingerprint_bank(bank)
    samples = fixed_samples()
    validations = validation_inputs()
    for item in validations:
        item["expected_numbers"] = validate(item["input"])
    isolated = isolated_inputs()
    for item in isolated:
        item["expected_answer"] = isolated_answer(item["input"])
    collectors = collector_cases()
    for item in collectors:
        item["expected"] = collect_expected(item, Collector, TestFailure)

    scoring = [
        {"id": item["id"], "numbers": item["numbers"], "expected_scores": scores(item["numbers"], bank)}
        for item in samples
    ]
    analysis = [
        {
            "id": f"first_{count}_samples",
            "outputs": [item["text"] for item in samples[:count]],
            "expected": analyze([item["text"] for item in samples[:count]], (bank, bank_version)),
        }
        for count in (1, 2, 3)
    ]
    analysis.append({"id": "no_valid_output", "outputs": ["1,2,3"], "expected": None})

    blobs = [
        {"id": "empty_blob", "raw_hex": b"".hex()},
        {"id": "ascii_blob", "raw_hex": bytes([104, 101, 108, 108, 111, 10]).hex()},
        {"id": "unicode_blob", "raw_hex": "ModelTrace fixture 你好".encode("utf-8").hex()},
    ]
    for item in blobs:
        item["expected_digest"] = git_blob_digest(bytes.fromhex(item["raw_hex"]))

    executes = stream_execute_cases()
    for item in executes:
        item["expected"] = execute_expected(item, execute, TestFailure)

    challenge_samples = list(challenges())
    if len(challenge_samples) != 3 or len({count for count, _ in challenge_samples}) != 3:
        raise AssertionError("pinned Python challenge generator must return three unique lengths")

    return {
        "schema_version": 1,
        "reference_sha": REFERENCE_SHA,
        "synthetic_only": True,
        "modeltrace": {
            "bank_version": bank_version,
            "validation_cases": validations,
            "scoring_cases": scoring,
            "samples": samples,
            "analysis_cases": analysis,
            "isolated_answer_cases": isolated,
            "challenge_expectations": {"count": 3, "minimum_length": 292, "maximum_length": 332},
        },
        "fingerprint_bank": {
            "validation_cases": bank_validation_cases(bank, validate_fingerprint_bank),
            "git_ref_cases": git_ref_inputs(),
            "git_blob_cases": blobs,
            "cache_expectations": {
                "valid_read_only_source": "cache",
                "read_only_absent_state_files": [],
                "lost_cache_error": {"status": 503, "detail": "控制状态文件丢失"},
                "corrupt_json_error": {"status": 503, "detail": "控制状态文件无效"},
                "corrupt_cache_error": {"status": 503, "detail": "指纹库缓存损坏"},
                "incompatible_cache_error": {"status": 503, "detail": "指纹库缓存不兼容"},
            },
        },
        "target_identity_cases": target_identity_inputs(codex_identity),
        "target_signature_cases": target_signature_inputs(target_signature),
        "failure_cases": failure_inputs(failure),
        "collector_cases": collectors,
        "execute_cases": executes,
    }


def main() -> None:
    output = ROOT / "tests" / "contracts" / "modeltrace-fixtures.json"
    with tempfile.TemporaryDirectory(prefix="sub2ops-modeltrace-040-") as temporary:
        app_root = Path(temporary)
        export_pinned_app(app_root, ROOT)
        fixture = build_fixtures(app_root)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + chr(10), encoding="utf-8")
    print(f"wrote {output.relative_to(ROOT)} from {REFERENCE_SHA}")


if __name__ == "__main__":
    main()
