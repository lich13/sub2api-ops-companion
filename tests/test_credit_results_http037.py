from __future__ import annotations

import json
import threading
import unittest
from datetime import timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from app.bark import BarkNotifier

from tests.test_auto_reset import (
    ACCOUNT_ID,
    ADMIN_TOKEN,
    AutoResetFixture,
    LocalCreditServer,
    NOW,
    quota_result,
    wham,
)


RAW_MODEL_OUTPUT = "raw-model-output-fixture"
BARK_DEVICE_KEY = "bark-loopback-device"


class BarkSuccessServer:
    """A loopback Bark endpoint that records accepted v2 payloads."""

    def __init__(self, fixture: AutoResetFixture):
        self.requests: list[dict] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                size = int(self.headers.get("Content-Length", "0"))
                raw = self.rfile.read(size)
                owner.requests.append({
                    "path": self.path,
                    "headers": dict(self.headers),
                    "payload": json.loads(raw),
                })
                body = b'{"code":200}'
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        fixture.allowed_ports.add(self.server.server_port)
        self.thread = threading.Thread(
            target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True
        )
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)


class CreditResultHttp037Tests(AutoResetFixture):
    """Exercise durable credit result notices through real local HTTP edges."""

    def setUp(self):
        super().setUp()
        self.credit_server = LocalCreditServer(self)
        self.addCleanup(self.credit_server.close)
        self.bark_server = BarkSuccessServer(self)
        self.addCleanup(self.bark_server.close)
        self.base_url = self.credit_server.base_url
        self.make_controller(real_http=True)

    def _fresh_real_http_case(self):
        self.fresh_case()
        self.credit_server.requests.clear()
        self.credit_server.quota_reads = 0
        self.bark_server.requests.clear()
        self.base_url = self.credit_server.base_url
        self.make_controller(real_http=True)
        self.settings.bark_enabled = True
        self.settings.bark_config_valid = True
        self.settings.bark_device_key = BARK_DEVICE_KEY
        self.settings.bark_server_url = self.bark_server.base_url
        return BarkNotifier(self.settings)

    def _prepare_episode(self, outcome: str):
        self.db.raw.execute("DELETE FROM ops_error_logs")
        self.db.event(record_id=self.case_number, at=self.now - timedelta(minutes=1))
        observed = self.now - timedelta(minutes=2)
        self.store.commit(results={
            ACCOUNT_ID: quota_result(
                wham(self.now, five=20, seven=100, observed=observed,
                     seven_reset_at=self.reset_seven_reset_at),
                self.row,
                observed,
            )
        })
        if outcome == "not_executed":
            self.row["extra"]["codex_reset_credit_snapshot"] = {
                "available_count": 0,
                "fetched_at": observed.isoformat(),
                "credits": [],
            }
        elif outcome == "consume_failed":
            # LocalCreditServer wraps this native receipt in the real admin
            # envelope; execute_credit_request then applies consumption_result.
            self.request_results.append({
                "data": {"code": "nothing_to_reset", "windows_reset": 0}
            })
        elif outcome == "unknown":
            self.request_results.append({
                "data": {"code": "already_redeemed", "windows_reset": 0}
            })
        elif outcome == "unrecovered":
            self.test_results.append({
                "success": False,
                "error_code": "http_400",
                "raw_output": RAW_MODEL_OUTPUT,
            })

    def _assert_bark_payload(self, outcome: str):
        self.assertEqual(len(self.bark_server.requests), 1)
        request = self.bark_server.requests[0]
        self.assertEqual(request["path"], "/push")
        self.assertEqual(request["headers"].get("Content-Type"), "application/json")
        payload = request["payload"]
        self.assertEqual(payload["device_key"], BARK_DEVICE_KEY)
        self.assertEqual(payload["level"], "critical")
        self.assertEqual(payload["sound"], "alarm")
        self.assertEqual(payload["group"], "oauth-reset-credit")
        serialized = json.dumps(payload, ensure_ascii=False)
        self.assertNotIn(ADMIN_TOKEN, serialized)
        self.assertNotIn("fixture-access-v1", serialized)
        self.assertNotIn(RAW_MODEL_OUTPUT, serialized)
        if outcome == "restored":
            self.assertEqual(payload["title"], "✅ OAuth 用卡成功，已恢复调度")
            self.assertIn("结果：用卡成功，已恢复调度", payload["body"])
        else:
            self.assertTrue(payload["title"].startswith("⚠️ OAuth "))
            self.assertNotEqual(payload["title"], "✅ OAuth 用卡成功，已恢复调度")
            self.assertIn("结果：", payload["body"])
            expected = {
                "not_executed": "未执行用卡",
                "consume_failed": "用卡明确失败",
                "unrecovered": "恢复未完成",
                "unknown": "消费结果不确定",
            }[outcome]
            self.assertIn(expected, payload["body"])

    def test_each_credit_result_is_delivered_once_and_survives_restart(self):
        cases = (
            ("restored", {"reset": 1, "test": 1}),
            ("not_executed", {"reset": 0, "test": 0}),
            ("consume_failed", {"reset": 1, "test": 0}),
            ("unrecovered", {"reset": 1, "test": 1}),
            ("unknown", {"reset": 1, "test": 0}),
        )
        for outcome, expected_calls in cases:
            with self.subTest(outcome=outcome):
                notifier = self._fresh_real_http_case()
                self._prepare_episode(outcome)
                self.run_once()

                task = self.state()
                self.assertEqual(task["outcome"], outcome)
                self.assertEqual(self.calls.count("reset"), expected_calls["reset"])
                self.assertEqual(self.calls.count("test"), expected_calls["test"])
                if outcome == "consume_failed":
                    self.assertEqual(task["receipt"]["business_code"], "nothing_to_reset")
                    self.assertEqual(task["consumption_outcome"], "not_consumed")
                if outcome == "unknown":
                    self.assertEqual(task["receipt"]["business_code"], "already_redeemed")
                    self.assertEqual(task["consumption_outcome"], "unknown")

                notices = self.store.snapshot()["scheduler"][str(ACCOUNT_ID)]["credit_result_notifications"]
                self.assertEqual(len(notices), 1)
                episode = task["episode"]
                notice = notices[episode]
                self.assertEqual(notice["status"], "pending")
                self.assertEqual(notice["outcome"], outcome)
                self.assertEqual(notice["attempts"], 0)

                self.controller.deliver_results(notifier)
                delivered = self.store.snapshot()["scheduler"][str(ACCOUNT_ID)]["credit_result_notifications"][episode]
                self.assertEqual(delivered["status"], "delivered")
                self.assertEqual(delivered["attempts"], 1)
                self._assert_bark_payload(outcome)

                calls_after_delivery = tuple(self.calls)
                self.controller.deliver_results(notifier)
                self.assertEqual(len(self.bark_server.requests), 1)
                self.assertEqual(tuple(self.calls), calls_after_delivery)

                self.make_controller(real_http=True)
                self.now += timedelta(hours=1)
                self.run_once()
                self.controller.deliver_results(notifier)
                self.assertEqual(len(self.bark_server.requests), 1)
                self.assertEqual(tuple(self.calls), calls_after_delivery)
                persisted = self.store.snapshot()["scheduler"][str(ACCOUNT_ID)]["credit_result_notifications"][episode]
                self.assertEqual(persisted["status"], "delivered")
                self.assertEqual(persisted["attempts"], 1)


if __name__ == "__main__":
    unittest.main()
