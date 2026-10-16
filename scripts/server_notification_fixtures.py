#!/usr/bin/env python3
"""Build Bark/capacity notification parity fixtures from the pinned 0.1.39 app.

This script only exports app/ from the pinned commit into a private temporary
directory, evaluates synthetic inputs with that implementation, and writes the
resulting fixture below tests/contracts/notifications/.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import socket
import stat
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
from server_reference import REFERENCE_SHA
FIXTURE_PATH = ROOT / "tests" / "contracts" / "notifications" / "notifications-fixtures.json"
NOW = "2026-09-04T03:00:00+00:00"
SAFE_DATABASE_URL = "postgresql://example.invalid/fixture"


def require(value: bool, message: str) -> None:
    if not value:
        raise RuntimeError(message)


def jsonable(value: Any) -> Any:
    if isinstance(value, datetime):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(key): jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(item) for item in value]
    return value


def export_pinned_app(root: Path) -> None:
    result = subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", REFERENCE_SHA, "--", "app"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    paths = [line for line in result.stdout.splitlines() if line.startswith("app/")]
    require(bool(paths), f"no pinned app tree at {REFERENCE_SHA}")
    for relative in paths:
        source = subprocess.run(
            ["git", "show", f"{REFERENCE_SHA}:{relative}"],
            cwd=ROOT,
            check=True,
            capture_output=True,
        )
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(source.stdout)


class FakeResponse:
    def __init__(self, payload: object, status: int = 200, raw: bytes | None = None) -> None:
        self.status = status
        self.payload = raw if raw is not None else json.dumps(payload).encode("utf-8")

    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, _limit: int = -1) -> bytes:
        return self.payload

    def getcode(self) -> int:
        return self.status


def fake_settings(url: str = "https://api.day.app", key: str = "fixture-device-key", enabled: bool = True) -> Any:
    return SimpleNamespace(
        bark_enabled=enabled,
        bark_config_valid=True,
        bark_device_key=key,
        bark_server_url=url,
    )


def base_event(status: str = "recovered", dedupe_key: str = "event:7") -> dict[str, Any]:
    return {
        "account_id": 7,
        "account_name": "oauth-seven",
        "plan_type": "plus",
        "status": status,
        "stage": "active_usage",
        "window_labels": ["5h", "7d"],
        "model_id": "gpt-5.6-luna",
        "error_code": "http_401",
        "error": "Authorization: Bearer [fixture-secret-marker]",
        "checked_at": NOW,
        "dedupe_key": dedupe_key,
    }


def error_row(error_id: int, **changes: Any) -> dict[str, Any]:
    row = {
        "id": error_id,
        "account_id": 1,
        "created_at": NOW,
        "error_owner": "provider",
        "error_phase": "upstream",
        "requested_model": "fixture-model",
        "model": "fixture-model",
        "upstream_model": "fixture-model",
        "upstream_error_message": "Our servers are currently overloaded. Please try again later.",
        "error_message": None,
        "error_body": None,
        "upstream_error_detail": None,
        "upstream_status_code": 200,
        "account_name": "fixture-account",
        "account_platform": "openai",
        "account_type": "oauth",
        "account_deleted_at": None,
    }
    row.update(changes)
    return row


def gateway_row(error_id: int, message: str, **changes: Any) -> dict[str, Any]:
    row = error_row(
        error_id,
        upstream_error_message=None,
        error_message=message,
        error_owner="platform",
        error_phase="internal",
        error_source="gateway",
        stream=True,
        error_body=json.dumps(
            {"error": {"code": "fixture-overloaded", "message": message, "type": "fixture-unavailable"}},
            ensure_ascii=False,
        ),
    )
    row.update(changes)
    return row


def load_settings_reference(settings_module: Any, root: Path, config_path: Path, extra: dict[str, str]) -> Any:
    env = {
        "DATABASE_URL": SAFE_DATABASE_URL,
        "BARK_CONFIG_PATH": str(config_path),
        "OAUTH_CONFIG_PATH": str(root / "missing-oauth-config.json"),
        "KEEPALIVE_CONFIG_PATH": str(root / "keepalive-state.json"),
        **extra,
    }
    with patch.dict(os.environ, env, clear=True):
        return settings_module.load_settings()


def compact_settings(value: Any) -> dict[str, Any]:
    return {
        "enabled": bool(value.bark_enabled),
        "config_valid": bool(value.bark_config_valid),
        "device_key": str(value.bark_device_key),
        "server_url": str(value.bark_server_url),
    }


def build_queue_references(
    app_main: Any,
    bark_module: Any,
    BarkNotifier: Any,
    OAuthStateStore: Any,
    settings_module: Any,
    temporary: Path,
) -> dict[str, Any]:
    class Capture:
        def __init__(self, responses: list[tuple[int, object]] | None = None) -> None:
            self.requests: list[dict[str, Any]] = []
            self.responses = list(responses or [])

        def open(self, request: Any, *, timeout: float) -> FakeResponse:
            self.requests.append(
                {
                    "url": request.full_url,
                    "timeout": timeout,
                    "payload": json.loads(request.data.decode("utf-8")),
                }
            )
            status, payload = self.responses.pop(0) if self.responses else (200, {"code": 200})
            return FakeResponse(payload, status)

    def install(store: Any, notifier: Any, settings: Any) -> None:
        app_main.oauth_monitor = SimpleNamespace(store=store)
        app_main.bark_notifier = notifier
        app_main.settings = settings

    def store_at(name: str) -> Any:
        return OAuthStateStore(str(temporary / name))

    event = base_event()
    results: dict[str, Any] = {}

    store = store_at("queue-success.json")
    store.commit(pending_events={event["dedupe_key"]: event})
    settings = fake_settings()
    capture = Capture()
    install(store, BarkNotifier(settings, urlopen=capture.open), settings)
    asyncio.run(app_main.deliver_oauth_monitor_events([event]))
    asyncio.run(app_main.deliver_oauth_monitor_events([event]))
    state = store.snapshot()
    results["success_dedup"] = {
        "requests": len(capture.requests),
        "pending_count": len(state["pending_events"]),
        "notified_keys": state["scheduler"].get("7", {}).get("notified_keys", []),
        "suppressed": state["scheduler"].get("7", {}).get("last_notification_suppressed", False),
    }

    credit = base_event()
    credit.update(
        reset_credit={"consumed": True, "completed_at": NOW, "verification_method": "connection"},
        test_success=True,
        recovered_at=NOW,
    )
    store = store_at("queue-credit.json")
    store.commit(pending_events={credit["dedupe_key"]: credit})
    capture = Capture()
    install(store, BarkNotifier(fake_settings(), urlopen=capture.open), fake_settings())
    asyncio.run(app_main.deliver_oauth_monitor_events([credit]))
    asyncio.run(app_main.deliver_oauth_monitor_events([credit]))
    state = store.snapshot()
    results["confirmed_credit"] = {
        "requests": len(capture.requests),
        "title": capture.requests[0]["payload"]["title"] if capture.requests else None,
        "pending_count": len(state["pending_events"]),
        "notified_keys": state["scheduler"].get("7", {}).get("notified_keys", []),
    }

    store = store_at("queue-failure.json")
    store.commit(pending_events={event["dedupe_key"]: event})
    capture = Capture([(200, {"code": 500}), (200, {"code": 500})])
    install(store, BarkNotifier(fake_settings(), urlopen=capture.open), fake_settings())
    asyncio.run(app_main.deliver_oauth_monitor_events([event]))
    state = store.snapshot()
    results["failure_retry"] = {
        "requests": len(capture.requests),
        "pending_count": len(state["pending_events"]),
        "scheduler": state["scheduler"],
    }

    revoked = base_event(dedupe_key="recovery:7:fixture:failure:synthetic")
    revoked["stage"] = "recovery"
    store = store_at("queue-revoked.json")
    store.commit(pending_events={revoked["dedupe_key"]: revoked})
    capture = Capture()
    settings = fake_settings()
    install(store, BarkNotifier(settings, urlopen=capture.open), settings)
    store.manual_control({"id": 7}, False, datetime.now(timezone.utc))
    asyncio.run(app_main.deliver_oauth_monitor_events([revoked]))
    results["manual_revoke"] = {"requests": len(capture.requests), "pending_count": len(store.snapshot()["pending_events"])}

    store = store_at("queue-disabled.json")
    store.commit(pending_events={event["dedupe_key"]: event})
    settings = fake_settings(enabled=False)
    capture = Capture()
    install(store, BarkNotifier(settings, urlopen=capture.open), settings)
    asyncio.run(app_main.deliver_oauth_monitor_events([event]))
    state = store.snapshot()
    results["explicitly_disabled"] = {
        "requests": len(capture.requests),
        "pending_count": len(state["pending_events"]),
        "suppressed": state["scheduler"].get("7", {}).get("last_notification_suppressed", False),
    }

    store = store_at("queue-invalid.json")
    store.commit(pending_events={event["dedupe_key"]: event})
    settings = fake_settings()
    settings.bark_config_valid = False
    capture = Capture()
    install(store, BarkNotifier(settings, urlopen=capture.open), settings)
    asyncio.run(app_main.deliver_oauth_monitor_events([event]))
    results["invalid_config"] = {"requests": len(capture.requests), "pending_count": len(store.snapshot()["pending_events"])}

    invalid_environment = load_settings_reference(
        settings_module,
        temporary,
        temporary / "missing-invalid-environment-config.json",
        {
            "BARK_ENABLED": "treu",
            "BARK_DEVICE_KEY": "fixture-environment-key",
            "BARK_SERVER_URL": "https://example.invalid",
        },
    )
    store = store_at("queue-invalid-environment.json")
    store.commit(pending_events={event["dedupe_key"]: event})
    capture = Capture()
    install(store, BarkNotifier(invalid_environment, urlopen=capture.open), invalid_environment)
    asyncio.run(app_main.deliver_oauth_monitor_events([event]))
    results["invalid_environment"] = {
        "config_valid": bool(invalid_environment.bark_config_valid),
        "requests": len(capture.requests),
        "pending_count": len(store.snapshot()["pending_events"]),
    }

    malformed = {"account_id": "fixture-invalid-id", "status": "recovered"}
    capture = Capture()
    settings = fake_settings()
    notifier = BarkNotifier(settings, urlopen=capture.open)
    delivered = notifier.notify_oauth_monitor_events([malformed, event])
    results["malformed_then_valid"] = {
        "requests": len(capture.requests),
        "delivered": delivered,
        "delivered_dedupe_keys": [item.get("dedupe_key") for item in delivered],
        "malformed_input": malformed,
        "valid_event": event,
    }

    long_event = base_event()
    long_event["window_labels"] = [
        f"window-{index}-" + "长" * 70 for index in range(50)
    ]
    store = store_at("queue-multisegment-failure.json")
    store.commit(pending_events={long_event["dedupe_key"]: long_event})
    capture = Capture([(200, {"code": 200}), (200, {"code": 500})])
    settings = fake_settings()
    install(store, BarkNotifier(settings, urlopen=capture.open), settings)
    asyncio.run(app_main.deliver_oauth_monitor_events([long_event]))
    results["multisegment_failure"] = {
        "event": long_event,
        "requests": len(capture.requests),
        "pending_count": len(store.snapshot()["pending_events"]),
        "notified_keys": store.snapshot()["scheduler"].get("7", {}).get("notified_keys", []),
        "titles": [request["payload"]["title"] for request in capture.requests],
        "body_byte_lengths": [
            len(request["payload"]["body"].encode("utf-8"))
            for request in capture.requests
        ],
        "total_parts": len(bark_module.bark_body_parts(bark_module.oauth_event_message(long_event)[1])),
    }
    return results


def build_bark_config_save_reference(
    app_main: Any,
    BarkNotifier: Any,
    temporary: Path,
) -> dict[str, Any]:
    from app import config_service as config_service_module

    root = temporary / "config-service-save"
    root.mkdir(parents=True, exist_ok=True)
    config_path = root / "bark-config.json"
    settings = SimpleNamespace(
        bark_enabled=True,
        bark_config_valid=True,
        bark_device_key="fixture-initial-key",
        bark_server_url="https://api.day.app",
        bark_config_path=str(config_path),
        audit_path=str(root / "audit.jsonl"),
    )
    original_settings, original_notifier = app_main.settings, app_main.bark_notifier
    app_main.settings = settings
    app_main.bark_notifier = BarkNotifier(settings)

    class FixedDateTime:
        @staticmethod
        def now(_tz: Any = None) -> datetime:
            return datetime.fromisoformat(NOW)

    async def save_config() -> dict[str, Any]:
        service = config_service_module.ConfigService(app_main)
        await service.save(
            "bark",
            {
                "enabled": True,
                "device_key": "fixture-save-one",
                "server_url": "http://127.0.0.1:50126",
            },
            "fixture-admin",
        )
        return await service.save(
            "bark", {"device_key": "fixture-save-two"}, "fixture-admin"
        )

    try:
        with patch.object(config_service_module, "datetime", FixedDateTime):
            panel = asyncio.run(save_config())
        persisted = json.loads(config_path.read_text(encoding="utf-8"))
        mode_octal = format(stat.S_IMODE(config_path.stat().st_mode), "04o")
        temporary_files = sorted(
            path.name
            for path in config_path.parent.iterdir()
            if path.name.startswith(f".{config_path.name}.") and path.name.endswith(".tmp")
        )
    finally:
        app_main.settings, app_main.bark_notifier = original_settings, original_notifier
    return {
        "persisted": persisted,
        "panel": jsonable(panel),
        "mode_octal": mode_octal,
        "temporary_files": temporary_files,
    }


def build_fixtures(reference_root: Path, work: Path) -> dict[str, Any]:
    sys.path.insert(0, str(reference_root))
    from app import bark as bark_module
    from app import capacity_alerts as capacity_module
    from app import settings as settings_module
    from app.bark import BarkNotifier
    from app.oauth_monitor import OAuthStateStore

    # main.py captures settings on import. Keep its import isolated from the
    # user's environment and point every optional file at this runner temp dir.
    main_env = {
        "DATABASE_URL": SAFE_DATABASE_URL,
        "BARK_CONFIG_PATH": str(work / "main-bark-config.json"),
        "OAUTH_CONFIG_PATH": str(work / "main-oauth-config.json"),
        "USAGE_QUERY_STATE_PATH": str(work / "main-usage-state.json"),
        "AUDIT_PATH": str(work / "main-audit.jsonl"),
        "KEEPALIVE_CONFIG_PATH": str(work / "keepalive-state.json"),
    }
    with patch.dict(os.environ, main_env, clear=True):
        from app import main as app_main

    fixture: dict[str, Any] = {
        "reference": {"sha": REFERENCE_SHA},
        "bark": {},
        "capacity": {},
        "gateway": {},
    }

    bark_cases = {
        "normalize_inputs": ["https://api.day.app///", "https://fixture.example.invalid/root/"],
        "normalized": [
            bark_module.normalize_bark_server_url("https://api.day.app///"),
            bark_module.normalize_bark_server_url("https://fixture.example.invalid/root/"),
        ],
        "push_urls": [
            bark_module.bark_push_url("https://fixture.example.invalid/root/"),
            bark_module.bark_push_url("https://fixture.example.invalid/自建/"),
            bark_module.bark_push_url("https://fixture.example.invalid/%E8%87%AA%E5%BB%BA/"),
        ],
        "allowed_urls": [
            "https://fixture.example.invalid",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ],
        "rejected_urls": [
            "http://fixture.example.invalid",
            "ftp://127.0.0.1",
            "https://fixture-user@fixture.example.invalid",
            "https://fixture.example.invalid?device_key=fixture-value",
            "https://fixture.example.invalid/#fragment",
            " https://fixture.example.invalid/has space ",
            "https://fixture.example.invalid/bad%escape",
            "https://fixture.example.invalid/bad\x00path",
            "not-a-url",
        ],
    }
    bark_cases["rejected_errors"] = []
    for value in bark_cases["rejected_urls"]:
        try:
            bark_module.normalize_bark_server_url(value)
            bark_cases["rejected_errors"].append(None)
        except ValueError as error:
            bark_cases["rejected_errors"].append(str(error))

    payload_text = "前缀" + "汉字🙂" * 700 + "suffix"
    bark_cases["payload"] = {
        "text": payload_text,
        "chunks": bark_module.split_utf8_chunks(payload_text),
        "parts": bark_module.bark_body_parts(payload_text),
        "max_bytes": 2400,
    }
    message_events = []
    for status in ("recovered", "test_failed", "recovery_failed", "auth_failed"):
        item = base_event(status)
        message_events.append({"id": status, "event": item, "expected": list(bark_module.oauth_event_message(item))})
    confirmed = base_event()
    confirmed.update(
        reset_credit={"consumed": True, "completed_at": NOW, "verification_method": "connection"},
        test_success=True,
        recovered_at=NOW,
    )
    incomplete = base_event()
    incomplete["reset_credit"] = {"consumed": True, "completed_at": NOW}
    message_events.extend(
        [
            {"id": "confirmed_credit", "event": confirmed, "expected": list(bark_module.oauth_event_message(confirmed))},
            {"id": "incomplete_credit", "event": incomplete, "expected": list(bark_module.oauth_event_message(incomplete))},
        ]
    )
    bark_cases["message_cases"] = message_events

    sanitize_input = "\n".join(
        (
            '{"api_key":"fixture-json-api-key","other":"ok"}',
            "headers={'X-Api-Key': 'fixture-header-key'}",
            "Authorization: Basic fixture-basic-auth-value",
            "Cookie: session=fixture-cookie-session; refresh=fixture-cookie-refresh",
            "url=https://fixture-user:fixture-password@example.invalid/path?access_token=fixture-query-token",
            "private_key=fixture-private-key-value",
            "Incorrect API key provided: fixture-error-key",
            "token is narrative-fixture-token",
            "unlabelled sk-proj-fixture-marker",
            "-----BEGIN PRIVATE KEY----- fixture-pem-marker -----END PRIVATE KEY-----",
        )
    )
    bark_cases["sanitization"] = {
        "input": sanitize_input,
        "expected": bark_module.sanitize_error_text(sanitize_input, 400),
        "long_utf8_expected_length_at_most": 400,
        "long_utf8": bark_module.sanitize_error_text("汉" * 400, 400),
    }

    transport_cases: dict[str, Any] = {}
    for case_id, status, payload in (
        ("http_status", 503, {"code": 200}),
        ("business_code", 200, {"code": 500}),
        ("string_code", 200, {"code": "200"}),
    ):
        notifier = BarkNotifier(fake_settings(), urlopen=lambda *_args, _status=status, _payload=payload, **_kwargs: FakeResponse(_payload, _status))
        outcome = notifier.push("fixture-title", "fixture-body")
        transport_cases[case_id] = {
            "success": outcome.success,
            "error_code": outcome.error_code,
            "sent_parts": outcome.sent_parts,
            "total_parts": outcome.total_parts,
        }
    invalid_json = BarkNotifier(
        fake_settings(), urlopen=lambda *_args, **_kwargs: FakeResponse({}, raw=b"not-json")
    ).push("fixture-title", "fixture-body")
    transport_cases["invalid_json"] = {"success": invalid_json.success, "error_code": invalid_json.error_code}
    missing = BarkNotifier(fake_settings(key=""), urlopen=lambda *_args, **_kwargs: FakeResponse({"code": 200})).push("fixture-title", "fixture-body")
    invalid_url = BarkNotifier(fake_settings(url="http://fixture.example.invalid"), urlopen=lambda *_args, **_kwargs: FakeResponse({"code": 200})).push("fixture-title", "fixture-body")
    transport_cases["missing_key"] = {"success": missing.success, "error_code": missing.error_code}
    transport_cases["invalid_url"] = {"success": invalid_url.success, "error_code": invalid_url.error_code}
    timeout_result = BarkNotifier(fake_settings(), urlopen=lambda *_args, **_kwargs: (_ for _ in ()).throw(socket.timeout("fixture slow"))).push("fixture-title", "fixture-body")
    request_result = BarkNotifier(fake_settings(), urlopen=lambda *_args, **_kwargs: (_ for _ in ()).throw(OSError("fixture unavailable"))).push("fixture-title", "fixture-body")
    transport_cases["timeout"] = {"success": timeout_result.success, "error_code": timeout_result.error_code}
    transport_cases["request_error"] = {"success": request_result.success, "error_code": request_result.error_code}
    bark_cases["transport_expected"] = transport_cases

    monitor_timeouts: list[tuple[float, dict[str, Any]]] = []

    def timeout_capture(request: Any, *, timeout: float) -> FakeResponse:
        monitor_timeouts.append((timeout, json.loads(request.data.decode("utf-8"))))
        return FakeResponse({"code": 200})

    timeout_notifier = BarkNotifier(fake_settings(), urlopen=timeout_capture)
    timeout_delivered = timeout_notifier.notify_oauth_monitor_events([base_event()])
    test_push = timeout_notifier.push_test()
    bark_cases["timeout_calls"] = {
        "timeouts": [value for value, _ in monitor_timeouts],
        "monitor_delivered_count": len(timeout_delivered),
        "test_success": test_push.success,
        "test_payload": monitor_timeouts[-1][1] if monitor_timeouts else {},
    }

    long_event = base_event()
    long_event["window_labels"] = [f"fixture-window-{index}-" + "长" * 70 for index in range(50)]
    multi_responses = [(200, {"code": 200}), (200, {"code": 500})]
    multi_calls: list[dict[str, Any]] = []

    def multi_capture(request: Any, *, timeout: float) -> FakeResponse:
        del timeout
        multi_calls.append(json.loads(request.data.decode("utf-8")))
        status, payload = multi_responses.pop(0) if multi_responses else (200, {"code": 200})
        return FakeResponse(payload, status)

    multi_delivered = BarkNotifier(fake_settings(), urlopen=multi_capture).notify_oauth_monitor_events([long_event])
    bark_cases["multisegment_failure"] = {
        "delivered_count": len(multi_delivered),
        "request_count": len(multi_calls),
        "total_parts": len(bark_module.bark_body_parts(bark_module.oauth_event_message(long_event)[1])),
        "payloads": multi_calls,
    }

    class DroppedResponse(FakeResponse):
        def __init__(self) -> None:
            super().__init__({"code": 200})

    malformed_calls: list[dict[str, Any]] = []

    def malformed_capture(request: Any, *, timeout: float) -> DroppedResponse:
        del timeout
        malformed_calls.append(json.loads(request.data.decode("utf-8")))
        return DroppedResponse()

    malformed = {"account_id": "fixture-invalid-id", "status": "recovered"}
    valid = base_event()
    delivered = BarkNotifier(fake_settings(), urlopen=malformed_capture).notify_oauth_monitor_events([malformed, valid])
    bark_cases["malformed_then_valid"] = {
        "request_count": len(malformed_calls),
        "delivered_dedupe_keys": [item.get("dedupe_key") for item in delivered],
    }

    with tempfile.TemporaryDirectory(prefix="notifications-reference-") as scratch:
        scratch_root = Path(scratch)
        config_path = scratch_root / "bark-config.json"
        config_path.write_text(
            json.dumps({"enabled": False, "device_key": "fixture-file-key", "server_url": "https://file.example.invalid"}),
            encoding="utf-8",
        )
        file_settings = load_settings_reference(
            settings_module,
            scratch_root,
            config_path,
            {"BARK_ENABLED": "true", "BARK_DEVICE_KEY": "fixture-env-key", "BARK_SERVER_URL": "https://env.example.invalid"},
        )
        bark_cases["settings_file_overrides_environment"] = compact_settings(file_settings)

        missing_settings = load_settings_reference(
            settings_module,
            scratch_root,
            scratch_root / "missing-bark-config.json",
            {"BARK_ENABLED": "true", "BARK_DEVICE_KEY": "fixture-env-key", "BARK_SERVER_URL": "https://env.example.invalid/"},
        )
        bark_cases["settings_environment_fallback"] = compact_settings(missing_settings)

        invalid_json_path = scratch_root / "invalid-bark-config.json"
        invalid_json_path.write_text("{broken", encoding="utf-8")
        invalid_json_settings = load_settings_reference(settings_module, scratch_root, invalid_json_path, {})
        bark_cases["invalid_existing_config"] = compact_settings(invalid_json_settings)

        invalid_bytes_path = scratch_root / "invalid-utf8-bark-config.json"
        invalid_bytes_path.write_bytes(b"\xff\xfe\x00")
        invalid_bytes_settings = load_settings_reference(settings_module, scratch_root, invalid_bytes_path, {})
        original_settings, original_notifier = app_main.settings, app_main.bark_notifier
        app_main.settings = invalid_bytes_settings
        app_main.bark_notifier = BarkNotifier(invalid_bytes_settings)
        try:
            invalid_bytes_panel = app_main.build_bark_config()
        finally:
            app_main.settings, app_main.bark_notifier = original_settings, original_notifier
        bark_cases["invalid_utf8_config"] = {
            "settings": compact_settings(invalid_bytes_settings),
            "panel_config_valid": invalid_bytes_panel["config_valid"],
        }

        invalid_schema_cases = [
            {"id": "enabled_wrong_type", "input": {"enabled": {"fixture": True}, "device_key": "fixture-key", "server_url": "https://api.day.app"}},
            {"id": "device_key_wrong_type", "input": {"enabled": False, "device_key": 17, "server_url": "https://api.day.app"}},
            {"id": "invalid_effective_url", "input": {"enabled": False, "device_key": "fixture-key", "server_url": "http://fixture.example.invalid"}},
        ]
        for case in invalid_schema_cases:
            path = scratch_root / f"{case['id']}.json"
            path.write_text(json.dumps(case["input"]), encoding="utf-8")
            settings_value = load_settings_reference(settings_module, scratch_root, path, {})
            case["expected_config_valid"] = bool(settings_value.bark_config_valid)
        bark_cases["invalid_schema_cases"] = invalid_schema_cases

        atomic_path = scratch_root / "atomic-save" / "bark-config.json"
        original_main_settings = app_main.settings
        app_main.settings = SimpleNamespace(bark_config_path=str(atomic_path))
        try:
            app_main.save_bark_runtime_config({"device_key": "fixture-save-one"})
            app_main.save_bark_runtime_config({"device_key": "fixture-save-two"})
        finally:
            app_main.settings = original_main_settings
        bark_cases["atomic_save"] = {
            "persisted": json.loads(atomic_path.read_text(encoding="utf-8")),
            "mode_octal": format(stat.S_IMODE(atomic_path.stat().st_mode), "04o"),
            "temporary_files": sorted(path.name for path in atomic_path.parent.iterdir() if path.name.endswith(".tmp")),
        }

        bark_cases["config_save"] = build_bark_config_save_reference(
            app_main, BarkNotifier, scratch_root
        )

        panel_path = scratch_root / "panel-bark-config.json"
        panel_path.write_text(
            json.dumps({"enabled": True, "device_key": "fixture-panel-device-key", "server_url": "https://panel.example.invalid"}),
            encoding="utf-8",
        )
        panel_settings = load_settings_reference(settings_module, scratch_root, panel_path, {})
        original_settings, original_notifier = app_main.settings, app_main.bark_notifier
        app_main.settings = panel_settings
        app_main.bark_notifier = BarkNotifier(panel_settings)
        try:
            bark_cases["panel"] = app_main.build_bark_config()
        finally:
            app_main.settings, app_main.bark_notifier = original_settings, original_notifier

    with tempfile.TemporaryDirectory(prefix="notifications-queue-reference-") as scratch:
        bark_cases["queue"] = build_queue_references(
            app_main, bark_module, BarkNotifier, OAuthStateStore, settings_module, Path(scratch)
        )

    capacity_messages = list(capacity_module.MESSAGES)
    capacity_cases = [
        {"id": "provider_plain_normalized", "row": error_row(1, upstream_error_message="  OUR SERVERS ARE CURRENTLY OVERLOADED. Please try again later!!!  ")},
        {"id": "provider_body_wrapped", "row": error_row(2, upstream_error_message=None, error_body=json.dumps({"error": {"message": capacity_messages[1]}}))},
        {"id": "provider_detail_object", "row": error_row(11, upstream_error_message=None, upstream_error_detail={"error": {"message": capacity_messages[2]}})},
        {"id": "provider_plain_concurrency", "row": error_row(12, upstream_error_message="  STREAM   DISCONNECTED BEFORE COMPLETION: concurrency limit exceeded for account, please retry later!!! ")},
        {"id": "provider_wrong_platform", "row": error_row(3, account_platform="grok")},
        {"id": "provider_wrong_type", "row": error_row(4, account_type="client_key")},
        {"id": "provider_deleted", "row": error_row(5, account_deleted_at=NOW)},
        {"id": "provider_wrong_owner", "row": error_row(6, error_owner="client")},
        {"id": "provider_wrong_phase", "row": error_row(7, error_phase="request")},
        {"id": "provider_body_plain_string", "row": error_row(10, upstream_error_message=None, error_body=capacity_messages[0])},
        {"id": "provider_body_nested_request", "row": error_row(8, upstream_error_message=None, error_body={"request": {"error": {"message": capacity_messages[0]}}})},
        {"id": "provider_body_nested_request_json", "row": error_row(9, upstream_error_message=None, error_body=json.dumps({"request": {"error": {"message": capacity_messages[1]}}}))},
        {"id": "provider_partial_concurrency", "row": error_row(13, upstream_error_message=None, error_body={"error": {"message": "stream disconnected before completion: concurrency limit exceeded for account"}})},
    ]
    for case in capacity_cases:
        case["expected"] = capacity_module.match_message(case["row"])
    fixture["capacity"] = {
        "messages": capacity_messages,
        "match_cases": capacity_cases,
        "retry_windows": list(capacity_module.RETRY_SECONDS),
    }

    gateway_cases = []
    for index, message in enumerate(capacity_messages, start=1):
        gateway_cases.append(
            {"id": f"gateway_phrase_{index}", "row": gateway_row(index, "  " + message.upper().replace(" ", " \n ") + "!!! ")}
        )
    gateway_cases.append(
        {"id": "gateway_body_after_message_clear", "row": gateway_row(10, capacity_messages[0], error_message=None)}
    )
    for event_type in ("response.failed", "response.incomplete"):
        gateway_cases.append(
            {
                "id": event_type.replace(".", "_"),
                "row": gateway_row(
                    11,
                    capacity_messages[0],
                    error_message=None,
                    error_body=json.dumps({"type": event_type, "response": {"error": {"message": capacity_messages[0]}}}),
                ),
            }
        )
    gateway_cases.append({"id": "gateway_stream_false_with_body", "row": gateway_row(12, capacity_messages[0], stream=False)})
    rejected_changes = [
        {"account_type": "key"},
        {"account_platform": "grok"},
        {"error_source": "client"},
        {"stream": False, "error_body": None},
        {"account_deleted_at": NOW},
        {"error_message": "Selected model is at capacity." , "error_body": None},
        {"error_message": "server_error", "error_body": '{"error":{"code":"server_error","message":"synthetic failure"}}'},
        {"error_message": None, "error_body": json.dumps({"request": {"error": {"message": capacity_messages[0]}}})},
    ]
    for index, change in enumerate(rejected_changes, start=20):
        gateway_cases.append({"id": f"gateway_false_positive_{index}", "row": gateway_row(index, capacity_messages[0], **change)})
    for case in gateway_cases:
        case["expected"] = capacity_module.match_message(case["row"])

    from app.desktop_api import ACCOUNT_SQL, ERROR_WHERE, error_dto

    gateway_record = {
        "id": 15886,
        "account_id": 387,
        "created_at": datetime(2026, 10, 1, tzinfo=timezone.utc),
        "resolved": True,
        "error_message": capacity_messages[0],
        "account_name": None,
    }
    fixture["gateway"] = {
        "match_cases": gateway_cases,
        "sql_contract": {
            "account_filter_formatted": ACCOUNT_SQL.format(filter="AND a.id=%(id)s"),
            "provider_history_visible": "e.error_owner='provider'" in ERROR_WHERE,
            "gateway_history_visible": "e.error_owner='platform'" in ACCOUNT_SQL,
            "dto_input": jsonable(gateway_record),
            "dto_expected": jsonable(error_dto(gateway_record)),
        },
    }
    fixture["bark"] = bark_cases
    return fixture


def main() -> int:
    if subprocess.run(
        ["git", "rev-parse", "--verify", f"{REFERENCE_SHA}^{{commit}}"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip() != REFERENCE_SHA:
        raise RuntimeError("pinned notification reference commit is unavailable")

    FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sub2ops-notification-reference-") as scratch:
        scratch_path = Path(scratch)
        reference_root = scratch_path / "reference"
        export_pinned_app(reference_root)
        fixture = build_fixtures(reference_root, scratch_path)

    serialized = json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    FIXTURE_PATH.write_text(serialized, encoding="utf-8")
    print(
        json.dumps(
            {
                "status": "fixtures_written",
                "reference_sha": REFERENCE_SHA,
                "path": str(FIXTURE_PATH.relative_to(ROOT)),
                "sha256": hashlib.sha256(FIXTURE_PATH.read_bytes()).hexdigest(),
                "capacity_cases": len(fixture["capacity"]["match_cases"]),
                "gateway_cases": len(fixture["gateway"]["match_cases"]),
                "bark_queue_cases": len(fixture["bark"]["queue"]),
            },
            ensure_ascii=False,
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
