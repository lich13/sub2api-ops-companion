from __future__ import annotations

import json
import os
import socket
import stat
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from app import main as main_module
from app.bark import (
    BarkNotifier,
    BarkRuntimeConfig,
    bark_body_parts,
    bark_push_url,
    normalize_bark_server_url,
    oauth_event_message,
    sanitize_error_text,
    split_utf8_chunks,
)
from app.oauth_monitor import OAuthStateStore
from app.settings import load_settings


NOW = "2026-09-04T03:00:00+00:00"


def bark_settings(server_url: str = "https://api.day.app") -> SimpleNamespace:
    return SimpleNamespace(
        bark_enabled=True,
        bark_device_key="device-secret",
        bark_server_url=server_url,
    )


def event(status: str = "recovered", *, dedupe_key: str = "event:7") -> dict[str, object]:
    return {
        "account_id": 7,
        "account_name": "oauth-seven",
        "plan_type": "plus",
        "status": status,
        "stage": "active_usage",
        "window_labels": ["5h", "7d"],
        "model_id": "gpt-5.6-luna",
        "error_code": "http_401",
        "error": "Authorization: Bearer top-secret-token",
        "checked_at": NOW,
        "dedupe_key": dedupe_key,
    }


class FakeResponse:
    def __init__(self, payload: object, status: int = 200) -> None:
        self.status = status
        self.payload = json.dumps(payload).encode("utf-8")

    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, _limit: int = -1) -> bytes:
        return self.payload

    def getcode(self) -> int:
        return self.status


class BarkCapture:
    def __init__(self, responses: list[tuple[int, object]] | None = None) -> None:
        self.requests: list[dict[str, object]] = []
        self.responses = list(responses or [(200, {"code": 200})])
        capture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length)
                capture.requests.append(
                    {
                        "path": self.path,
                        "content_type": self.headers.get("Content-Type"),
                        "payload": json.loads(raw.decode("utf-8")),
                    }
                )
                status, payload = (
                    capture.responses.pop(0)
                    if capture.responses
                    else (200, {"code": 200})
                )
                encoded = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def __enter__(self) -> BarkCapture:
        self.thread.start()
        return self

    def __exit__(self, *_args: object) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class BarkUrlTests(unittest.TestCase):
    def test_default_and_path_urls_normalize_before_push(self) -> None:
        self.assertEqual(normalize_bark_server_url("https://api.day.app///"), "https://api.day.app")
        self.assertEqual(
            bark_push_url("https://bark.example.com/root/"),
            "https://bark.example.com/root/push",
        )
        self.assertEqual(
            bark_push_url("https://bark.example.com/自建/"),
            "https://bark.example.com/%E8%87%AA%E5%BB%BA/push",
        )
        self.assertEqual(
            bark_push_url("https://bark.example.com/%E8%87%AA%E5%BB%BA/"),
            "https://bark.example.com/%E8%87%AA%E5%BB%BA/push",
        )

    def test_https_is_allowed_and_http_is_loopback_only(self) -> None:
        allowed = (
            "https://bark.example.com",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        )
        for value in allowed:
            with self.subTest(value=value):
                self.assertEqual(normalize_bark_server_url(value), value)
        rejected = (
            "http://bark.example.com",
            "ftp://127.0.0.1",
            "https://user:pass@example.com",
            "https://example.com?device_key=secret",
            "https://example.com/#fragment",
            " https://example.com/has space ",
            "https://example.com/bad%escape",
            "https://example.com/bad\x00path",
            "not-a-url",
        )
        for value in rejected:
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    normalize_bark_server_url(value)


class BarkPayloadTests(unittest.TestCase):
    def test_utf8_chunks_preserve_text_and_final_body_limit(self) -> None:
        text = "前缀" + "汉字🙂" * 700 + "suffix"
        chunks = split_utf8_chunks(text)
        self.assertEqual("".join(chunks), text)
        self.assertTrue(all(len(chunk.encode("utf-8")) <= 2400 for chunk in chunks))

        parts = bark_body_parts(text)
        rebuilt = "".join(part.split("\n", 1)[1] for part in parts)
        self.assertEqual(rebuilt, text)
        self.assertTrue(all(len(part.encode("utf-8")) <= 2400 for part in parts))
        self.assertTrue(parts[0].startswith(f"第 1/{len(parts)} 段\n"))

    def test_four_event_types_have_fixed_titles_and_required_fields(self) -> None:
        titles = {
            "recovered": "OAuth 账号额度已恢复可用",
            "test_failed": "OAuth 账号额度恢复后测试失败",
            "recovery_failed": "OAuth 账号自动恢复失败",
            "auth_failed": "OAuth 账号认证异常",
        }
        for status, expected_title in titles.items():
            with self.subTest(status=status):
                title, body = oauth_event_message(event(status))
                self.assertEqual(title, expected_title)
                for expected in (
                    "账号 ID：7",
                    "名称：oauth-seven",
                    "套餐：plus",
                    "窗口：5h / 7d",
                    "模型：gpt-5.6-luna",
                    "错误码：http_401",
                ):
                    self.assertIn(expected, body)
                self.assertNotIn("top-secret-token", body)

    def test_error_sanitization_covers_common_secret_shapes_and_utf8_limit(self) -> None:
        raw = "\n".join(
            (
                '{"api_key":"sk-live-json-secret","other":"ok"}',
                "headers={'X-Api-Key': 'sk-live-header-secret'}",
                "Authorization: Basic dXNlcjpwYXNzd29yZA==",
                "Cookie: session=cookie-secret; refresh=other-secret",
                "url=https://alice:password@example.com/path?access_token=query-secret",
                "private_key=private-secret",
                "Incorrect API key provided: sk-live-narrative-secret",
                "token is narrative-token-secret",
                "unlabelled sk-proj-known-secret-value",
                "-----BEGIN PRIVATE KEY----- secret-material -----END PRIVATE KEY-----",
            )
        )
        cleaned = sanitize_error_text(raw, 400)

        for secret in (
            "sk-live-json-secret",
            "sk-live-header-secret",
            "dXNlcjpwYXNzd29yZA==",
            "cookie-secret",
            "other-secret",
            "password",
            "query-secret",
            "private-secret",
            "sk-live-narrative-secret",
            "narrative-token-secret",
            "sk-proj-known-secret-value",
            "secret-material",
        ):
            self.assertNotIn(secret, cleaned)
        self.assertLessEqual(len(sanitize_error_text("汉" * 400).encode("utf-8")), 400)


class BarkTransportTests(unittest.TestCase):
    def test_loopback_capture_receives_real_v2_json_request(self) -> None:
        with BarkCapture() as capture:
            result = BarkNotifier(bark_settings(capture.url)).push("title", "body")

        self.assertTrue(result.success)
        self.assertEqual(len(capture.requests), 1)
        request = capture.requests[0]
        self.assertEqual(request["path"], "/push")
        self.assertEqual(request["content_type"], "application/json")
        self.assertEqual(
            request["payload"],
            {"device_key": "device-secret", "title": "title", "body": "body"},
        )

    def test_unicode_server_path_is_percent_encoded_for_real_request(self) -> None:
        with BarkCapture() as capture:
            result = BarkNotifier(bark_settings(f"{capture.url}/自建")).push("title", "body")

        self.assertTrue(result.success)
        self.assertEqual(capture.requests[0]["path"], "/%E8%87%AA%E5%BB%BA/push")

    def test_http_and_business_codes_must_both_succeed(self) -> None:
        cases = (
            (FakeResponse({"code": 200}, 503), "http_status"),
            (FakeResponse({"code": 500}), "bark_response_code"),
            (FakeResponse({"code": "200"}), "bark_response_code"),
        )
        for response, error_code in cases:
            with self.subTest(error_code=error_code):
                notifier = BarkNotifier(bark_settings(), urlopen=lambda *_a, **_k: response)
                result = notifier.push("title", "body")
                self.assertFalse(result.success)
                self.assertEqual(result.error_code, error_code)

    def test_redirect_is_not_followed_or_accepted_as_delivery(self) -> None:
        request_counts = {"post": 0, "get": 0}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                request_counts["post"] += 1
                self.send_response(302)
                self.send_header("Location", "/landing")
                self.end_headers()

            def do_GET(self) -> None:
                request_counts["get"] += 1
                encoded = b'{"code":200}'
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            result = BarkNotifier(
                bark_settings(f"http://127.0.0.1:{server.server_port}")
            ).push("title", "body")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

        self.assertFalse(result.success)
        self.assertEqual(result.error_code, "http_status")
        self.assertEqual(request_counts, {"post": 1, "get": 0})

    def test_missing_key_invalid_url_and_invalid_json_have_stable_codes(self) -> None:
        missing_key = bark_settings()
        missing_key.bark_device_key = ""
        invalid_url = bark_settings("http://example.com")

        self.assertEqual(
            BarkNotifier(missing_key).push("title", "body").error_code,
            "missing_device_key",
        )
        self.assertEqual(
            BarkNotifier(invalid_url).push("title", "body").error_code,
            "invalid_server_url",
        )

        class InvalidJsonResponse:
            status = 200

            def __enter__(self) -> InvalidJsonResponse:
                return self

            def __exit__(self, *_args: object) -> None:
                return None

            def read(self, _limit: int) -> bytes:
                return b"not-json"

        result = BarkNotifier(
            bark_settings(), urlopen=lambda *_a, **_k: InvalidJsonResponse()
        ).push("title", "body")
        self.assertFalse(result.success)
        self.assertEqual(result.error_code, "response_decode")

    def test_transport_failures_are_classified(self) -> None:
        failures = (
            (socket.timeout("slow"), "timeout"),
            (OSError("down"), "request_error"),
        )
        for failure, expected in failures:
            with self.subTest(expected=expected):
                def fail(*_args: object, **_kwargs: object) -> object:
                    raise failure

                result = BarkNotifier(bark_settings(), urlopen=fail).push("title", "body")
                self.assertFalse(result.success)
                self.assertEqual(result.error_code, expected)

    def test_runtime_config_swaps_never_mix_device_key_and_server(self) -> None:
        settings = bark_settings("https://a.example")
        settings.bark_device_key = "KEY_A"
        notifier = BarkNotifier(settings)
        original_settings = main_module.settings
        original_notifier = main_module.bark_notifier
        observed: list[tuple[str, str]] = []
        observed_lock = threading.Lock()
        start = threading.Barrier(2)

        def capture(request: object, *, timeout: float) -> FakeResponse:
            del timeout
            payload = json.loads(request.data.decode("utf-8"))
            with observed_lock:
                observed.append((request.full_url, payload["device_key"]))
            return FakeResponse({"code": 200})

        notifier._urlopen = capture

        def swap_configs() -> None:
            start.wait()
            for _ in range(1500):
                main_module.apply_bark_runtime_config(
                    {
                        "enabled": True,
                        "device_key": "KEY_B",
                        "server_url": "https://b.example",
                    }
                )
                main_module.apply_bark_runtime_config(
                    {
                        "enabled": True,
                        "device_key": "KEY_A",
                        "server_url": "https://a.example",
                    }
                )

        main_module.settings = settings
        main_module.bark_notifier = notifier
        thread = threading.Thread(target=swap_configs)
        try:
            thread.start()
            start.wait()
            for _ in range(1500):
                self.assertTrue(notifier.push("title", "body").success)
            thread.join(timeout=5)
        finally:
            thread.join(timeout=5)
            main_module.settings = original_settings
            main_module.bark_notifier = original_notifier

        self.assertFalse(thread.is_alive())
        self.assertTrue(observed)
        self.assertTrue(
            all(
                pair
                in {
                    ("https://a.example/push", "KEY_A"),
                    ("https://b.example/push", "KEY_B"),
                }
                for pair in observed
            )
        )

    def test_invalid_runtime_config_blocks_test_push(self) -> None:
        settings = bark_settings()
        settings.bark_config_valid = False

        def unexpected_request(*_args: object, **_kwargs: object) -> object:
            self.fail("invalid config must not make an HTTP request")

        result = BarkNotifier(settings, urlopen=unexpected_request).push_test()

        self.assertFalse(result.success)
        self.assertEqual(result.error_code, "invalid_config")

    def test_monitor_and_test_push_use_three_and_eight_second_timeouts(self) -> None:
        calls: list[tuple[dict[str, object], float]] = []

        def capture(request: object, *, timeout: float) -> FakeResponse:
            calls.append((json.loads(request.data.decode("utf-8")), timeout))
            return FakeResponse({"code": 200})

        notifier = BarkNotifier(bark_settings(), urlopen=capture)
        delivered = notifier.notify_oauth_monitor_events([event()])
        test_result = notifier.push_test()

        self.assertEqual(delivered, [event()])
        self.assertTrue(test_result.success)
        self.assertEqual([item[1] for item in calls], [3, 8])
        self.assertEqual(calls[1][0]["title"], "Sub2API Ops Companion")
        self.assertEqual(calls[1][0]["body"], "Bark 推送通道正常。")

    def test_multisegment_failure_does_not_deliver_event(self) -> None:
        long_event = event()
        long_event["window_labels"] = [f"window-{index}-" + "长" * 70 for index in range(50)]
        with BarkCapture([(200, {"code": 200}), (200, {"code": 500})]) as capture:
            delivered = BarkNotifier(bark_settings(capture.url)).notify_oauth_monitor_events(
                [long_event]
            )

        self.assertEqual(delivered, [])
        self.assertEqual(len(capture.requests), 2)
        total = len(bark_body_parts(oauth_event_message(long_event)[1]))
        for index, request in enumerate(capture.requests, start=1):
            payload = request["payload"]
            self.assertEqual(payload["title"], f"OAuth 账号额度已恢复可用 ({index}/{total})")
            self.assertLessEqual(len(payload["body"].encode("utf-8")), 2400)

    def test_malformed_event_does_not_block_a_later_valid_event(self) -> None:
        malformed = {"account_id": "oops", "status": "recovered"}
        with BarkCapture() as capture:
            delivered = BarkNotifier(bark_settings(capture.url)).notify_oauth_monitor_events(
                [malformed, event()]
            )

        self.assertEqual(delivered, [event()])
        self.assertEqual(len(capture.requests), 1)


class BarkConfigTests(unittest.TestCase):
    def test_json_config_overrides_environment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bark-config.json"
            path.write_text(
                json.dumps(
                    {
                        "enabled": False,
                        "device_key": "file-key",
                        "server_url": "https://file.example.com",
                    }
                ),
                encoding="utf-8",
            )
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "BARK_CONFIG_PATH": str(path),
                    "BARK_ENABLED": "true",
                    "BARK_DEVICE_KEY": "env-key",
                    "BARK_SERVER_URL": "https://env.example.com",
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertFalse(loaded.bark_enabled)
        self.assertEqual(loaded.bark_device_key, "file-key")
        self.assertEqual(loaded.bark_server_url, "https://file.example.com")
        self.assertTrue(loaded.bark_config_valid)

    def test_environment_precedes_defaults_when_file_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "missing.json"
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "BARK_CONFIG_PATH": str(path),
                    "BARK_ENABLED": "true",
                    "BARK_DEVICE_KEY": "env-key",
                    "BARK_SERVER_URL": "https://env.example.com/",
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertTrue(loaded.bark_enabled)
        self.assertEqual(loaded.bark_device_key, "env-key")
        self.assertEqual(loaded.bark_server_url, "https://env.example.com/")
        self.assertTrue(loaded.bark_config_valid)

    def test_invalid_existing_config_is_not_treated_as_explicitly_disabled(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bark-config.json"
            path.write_text("{broken", encoding="utf-8")
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "BARK_CONFIG_PATH": str(path),
                },
                clear=True,
            ):
                loaded = load_settings()

        self.assertFalse(loaded.bark_config_valid)

    def test_non_utf8_existing_config_is_invalid_without_crashing(self) -> None:
        original_settings = main_module.settings
        original_notifier = main_module.bark_notifier
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bark-config.json"
            path.write_bytes(b"\xff\xfe\x00")
            with patch.dict(
                os.environ,
                {
                    "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                    "OPS_SESSION_SECRET": "secret",
                    "BARK_CONFIG_PATH": str(path),
                },
                clear=True,
            ):
                loaded = load_settings()
            main_module.settings = loaded
            main_module.bark_notifier = BarkNotifier(loaded)
            try:
                panel = main_module.build_bark_config()
            finally:
                main_module.settings = original_settings
                main_module.bark_notifier = original_notifier

        self.assertFalse(loaded.bark_config_valid)
        self.assertFalse(panel["config_valid"])

    def test_invalid_field_schema_and_effective_url_mark_config_invalid(self) -> None:
        configs = (
            {"enabled": {"typo": True}, "device_key": "key", "server_url": "https://api.day.app"},
            {"enabled": False, "device_key": 123, "server_url": "https://api.day.app"},
            {"enabled": False, "device_key": "key", "server_url": "http://example.com"},
        )
        for config in configs:
            with self.subTest(config=config), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "bark-config.json"
                path.write_text(json.dumps(config), encoding="utf-8")
                with patch.dict(
                    os.environ,
                    {
                        "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                        "OPS_SESSION_SECRET": "secret",
                        "BARK_CONFIG_PATH": str(path),
                    },
                    clear=True,
                ):
                    loaded = load_settings()

                self.assertFalse(loaded.bark_config_valid)

    def test_atomic_save_is_0600_and_leaves_no_temporary_file(self) -> None:
        original = main_module.settings
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "bark-config.json"
            main_module.settings = SimpleNamespace(bark_config_path=str(path))
            try:
                main_module.save_bark_runtime_config({"device_key": "one"})
                main_module.save_bark_runtime_config({"device_key": "two"})
            finally:
                main_module.settings = original

            persisted = json.loads(path.read_text(encoding="utf-8"))
            mode = stat.S_IMODE(path.stat().st_mode)
            leftovers = list(root.glob(".bark-config.json.*.tmp"))

        self.assertEqual(persisted, {"device_key": "two"})
        self.assertEqual(mode, 0o600)
        self.assertEqual(leftovers, [])

    def test_panel_config_never_exposes_device_key(self) -> None:
        original = main_module.settings
        original_notifier = main_module.bark_notifier
        secret = "never-render-this-device-key"
        with tempfile.TemporaryDirectory() as directory:
            main_module.settings = SimpleNamespace(
                bark_config_path=str(Path(directory) / "bark-config.json"),
                bark_enabled=True,
                bark_device_key=secret,
                bark_server_url="https://api.day.app",
                bark_config_valid=True,
            )
            main_module.bark_notifier = BarkNotifier(main_module.settings)
            try:
                panel = main_module.build_bark_config()
            finally:
                main_module.settings = original
                main_module.bark_notifier = original_notifier

        self.assertNotIn(secret, json.dumps(panel, ensure_ascii=False))
        self.assertNotIn("device_key", panel)
        self.assertNotIn("server_url", panel)
        self.assertNotIn("https://api.day.app", json.dumps(panel, ensure_ascii=False))
        self.assertEqual(panel["device_key_status"], "已设置")


class BarkConfigRouteTests(unittest.IsolatedAsyncioTestCase):
    async def test_push_test_route_sends_real_json_to_loopback_capture(self) -> None:
        original_settings = main_module.settings
        original_notifier = main_module.bark_notifier
        with tempfile.TemporaryDirectory() as directory, BarkCapture() as capture:
            root = Path(directory)
            main_module.settings = SimpleNamespace(
                bark_enabled=False,
                bark_device_key="route-device-secret",
                bark_server_url=capture.url,
                base_path="/sub2ops",
                audit_path=str(root / "audit.jsonl"),
            )
            main_module.bark_notifier = BarkNotifier(main_module.settings)
            try:
                response = await main_module.bark_push_test("admin")
            finally:
                main_module.settings = original_settings
                main_module.bark_notifier = original_notifier

        self.assertEqual(response.status_code, 303)
        self.assertIn("Bark", response.headers["location"])
        self.assertEqual(len(capture.requests), 1)
        self.assertEqual(
            capture.requests[0]["payload"],
            {
                "device_key": "route-device-secret",
                "title": "Sub2API Ops Companion",
                "body": "Bark 推送通道正常。",
            },
        )
        self.assertNotIn("route-device-secret", response.headers["location"])

    async def test_blank_key_preserves_old_value_and_new_input_replaces_it(self) -> None:
        original = main_module.settings
        original_notifier = main_module.bark_notifier
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "bark-config.json"
            main_module.settings = SimpleNamespace(
                bark_config_path=str(path),
                bark_enabled=True,
                bark_device_key="old-key",
                bark_server_url="https://api.day.app",
                bark_config_valid=True,
                base_path="/sub2ops",
                audit_path=str(root / "audit.jsonl"),
            )
            main_module.bark_notifier = BarkNotifier(main_module.settings)
            try:
                first = await main_module.bark_config_save(
                    "admin", "on", "", "https://api.day.app/"
                )
                kept = json.loads(path.read_text(encoding="utf-8"))
                second = await main_module.bark_config_save(
                    "admin", "on", "new-key", "https://bark.example.com/"
                )
                replaced = json.loads(path.read_text(encoding="utf-8"))
                audit = (root / "audit.jsonl").read_text(encoding="utf-8")
            finally:
                main_module.settings = original
                main_module.bark_notifier = original_notifier

        self.assertEqual(first.status_code, 303)
        self.assertEqual(second.status_code, 303)
        self.assertEqual(kept["device_key"], "old-key")
        self.assertEqual(replaced["device_key"], "new-key")
        self.assertEqual(replaced["server_url"], "https://bark.example.com")
        self.assertNotIn("old-key", audit)
        self.assertNotIn("new-key", audit)

    async def test_missing_or_blank_server_url_preserves_custom_runtime_url(self) -> None:
        original = main_module.settings
        original_notifier = main_module.bark_notifier
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "bark-config.json"
            path.write_text(
                json.dumps(
                    {
                        "enabled": True,
                        "device_key": "old-key",
                        "server_url": "https://custom.example.com/root",
                    }
                ),
                encoding="utf-8",
            )
            main_module.settings = SimpleNamespace(
                bark_config_path=str(path),
                bark_enabled=True,
                bark_device_key="old-key",
                bark_server_url="https://custom.example.com/root",
                bark_config_valid=True,
                base_path="/sub2ops",
                audit_path=str(root / "audit.jsonl"),
            )
            main_module.bark_notifier = BarkNotifier(main_module.settings)
            try:
                missing = await main_module.bark_config_save("admin", "on", "")
                kept_missing = json.loads(path.read_text(encoding="utf-8"))
                blank = await main_module.bark_config_save("admin", "on", "", "   ")
                kept_blank = json.loads(path.read_text(encoding="utf-8"))
                explicit = await main_module.bark_config_save(
                    "admin", "on", "", "https://bark.example.com/"
                )
                replaced = json.loads(path.read_text(encoding="utf-8"))
            finally:
                main_module.settings = original
                main_module.bark_notifier = original_notifier

        self.assertEqual(missing.status_code, 303)
        self.assertEqual(blank.status_code, 303)
        self.assertEqual(explicit.status_code, 303)
        self.assertEqual(kept_missing["server_url"], "https://custom.example.com/root")
        self.assertEqual(kept_blank["server_url"], "https://custom.example.com/root")
        self.assertEqual(kept_missing["device_key"], "old-key")
        self.assertEqual(replaced["server_url"], "https://bark.example.com")

    async def test_invalid_server_url_does_not_write_config(self) -> None:
        original = main_module.settings
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "bark-config.json"
            main_module.settings = SimpleNamespace(
                bark_config_path=str(path),
                bark_enabled=False,
                bark_device_key="",
                bark_server_url="https://api.day.app",
                bark_config_valid=True,
                base_path="/sub2ops",
                audit_path=str(root / "audit.jsonl"),
            )
            try:
                response = await main_module.bark_config_save(
                    "admin", "on", "key", "http://example.com"
                )
            finally:
                main_module.settings = original

        self.assertEqual(response.status_code, 303)
        self.assertIn("Bark", response.headers["location"])
        self.assertFalse(path.exists())


class BarkQueueTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.original_settings = main_module.settings
        self.original_monitor = main_module.oauth_monitor
        self.original_notifier = main_module.bark_notifier
        self.tmpdir = tempfile.TemporaryDirectory()
        self.store = OAuthStateStore(str(Path(self.tmpdir.name) / "oauth-state.json"))
        self.pending = event()
        self.store.commit(pending_events={str(self.pending["dedupe_key"]): self.pending})
        main_module.oauth_monitor = SimpleNamespace(store=self.store)

    async def asyncTearDown(self) -> None:
        main_module.settings = self.original_settings
        main_module.oauth_monitor = self.original_monitor
        main_module.bark_notifier = self.original_notifier
        self.tmpdir.cleanup()

    async def test_success_is_acknowledged_and_telegram_has_no_monitor_sender(self) -> None:
        class SuccessNotifier:
            def runtime_config(self) -> BarkRuntimeConfig:
                return BarkRuntimeConfig(True, True, "key", "https://api.day.app")

            def notify_oauth_monitor_events(
                self,
                events: list[dict[str, object]],
                *,
                config: BarkRuntimeConfig,
            ) -> list[dict[str, object]]:
                del config
                return events

        main_module.settings = SimpleNamespace(bark_config_valid=True, bark_enabled=True)
        main_module.bark_notifier = SuccessNotifier()

        await main_module.deliver_oauth_monitor_events([self.pending])

        self.assertEqual(self.store.pending_events(), [])
        metadata = self.store.scheduler()[7]
        self.assertFalse(metadata["last_notification_suppressed"])
        self.assertFalse(hasattr(main_module.TelegramOpsBot, "notify_oauth_monitor_events"))

    async def test_failure_keeps_pending_for_retry(self) -> None:
        class FailedNotifier:
            def runtime_config(self) -> BarkRuntimeConfig:
                return BarkRuntimeConfig(True, True, "key", "https://api.day.app")

            def notify_oauth_monitor_events(
                self,
                _events: list[dict[str, object]],
                *,
                config: BarkRuntimeConfig,
            ) -> list[dict[str, object]]:
                del config
                return []

        main_module.settings = SimpleNamespace(bark_config_valid=True, bark_enabled=True)
        main_module.bark_notifier = FailedNotifier()

        await main_module.deliver_oauth_monitor_events([self.pending])

        self.assertEqual(self.store.pending_events(), [self.pending])
        self.assertEqual(self.store.scheduler(), {})

    async def test_explicitly_disabled_marks_pending_as_suppressed(self) -> None:
        main_module.settings = SimpleNamespace(bark_config_valid=True, bark_enabled=False)
        main_module.bark_notifier = BarkNotifier(
            SimpleNamespace(
                bark_config_valid=True,
                bark_enabled=False,
                bark_device_key="key",
                bark_server_url="https://api.day.app",
            )
        )

        await main_module.deliver_oauth_monitor_events([self.pending])

        self.assertEqual(self.store.pending_events(), [])
        self.assertTrue(self.store.scheduler()[7]["last_notification_suppressed"])

    async def test_invalid_config_preserves_pending(self) -> None:
        main_module.settings = SimpleNamespace(bark_config_valid=False, bark_enabled=False)
        main_module.bark_notifier = BarkNotifier(
            SimpleNamespace(
                bark_config_valid=False,
                bark_enabled=False,
                bark_device_key="key",
                bark_server_url="https://api.day.app",
            )
        )

        await main_module.deliver_oauth_monitor_events([self.pending])

        self.assertEqual(self.store.pending_events(), [self.pending])
        self.assertEqual(self.store.scheduler(), {})

    async def test_invalid_environment_enabled_preserves_pending(self) -> None:
        with tempfile.TemporaryDirectory() as directory, patch.dict(
            os.environ,
            {
                "DATABASE_URL": "postgresql://user:pass@127.0.0.1:5432/db",
                "OPS_SESSION_SECRET": "secret",
                "BARK_CONFIG_PATH": str(Path(directory) / "missing.json"),
                "BARK_ENABLED": "treu",
                "BARK_DEVICE_KEY": "key",
                "BARK_SERVER_URL": "https://api.day.app",
            },
            clear=True,
        ):
            loaded = load_settings()

        self.assertFalse(loaded.bark_config_valid)
        main_module.settings = loaded
        main_module.bark_notifier = BarkNotifier(loaded)
        await main_module.deliver_oauth_monitor_events([self.pending])

        self.assertEqual(self.store.pending_events(), [self.pending])
        self.assertEqual(self.store.scheduler(), {})


if __name__ == "__main__":
    unittest.main()
