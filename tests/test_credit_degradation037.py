from __future__ import annotations

import json

from app.bark import BarkNotifier
from app.model_detection import TARGET
from tests.test_auto_reset import ACCOUNT_ID, ADMIN_TOKEN
from tests.test_credit_results_http037 import BARK_DEVICE_KEY, BarkSuccessServer
from tests.test_recovery_models030 import OUTPUT, _RecoveryModelFixture


class CreditDegradationMerge037Tests(_RecoveryModelFixture):
    async def test_confirmed_credit_degradation_delivers_one_merged_bark(self):
        fixture = self.fixture
        bark_server = BarkSuccessServer(fixture)
        self.addCleanup(bark_server.close)
        fixture.settings.bark_enabled = True
        fixture.settings.bark_config_valid = True
        fixture.settings.bark_device_key = BARK_DEVICE_KEY
        fixture.settings.bark_server_url = bark_server.base_url
        notifier = BarkNotifier(fixture.settings)
        self.service.r.bark_notifier = notifier
        self.analysis_report = {"prediction": TARGET, "probability": .05, "used_outputs": 1}

        await self._card_cycle()
        job = self._latest()
        finished_job = await self._finish(job)

        self.assertEqual(finished_job["status"], "completed")
        self.assertEqual(finished_job["first_group_prediction"], TARGET)
        self.assertEqual(len(self.model_calls), 1)
        task = fixture.state()
        self.assertTrue(task["consumed"])
        self.assertTrue(task["attempt_at"])
        self.assertEqual(task["stage"], "finished")
        self.assertEqual(task["outcome"], "unrecovered")
        self.assertEqual(fixture.calls.count("reset"), 1)
        self.assertEqual(fixture.calls.count("model"), 1)
        self.assertNotIn("recover", fixture.calls)
        self.assertTrue(self.detection.mark(ACCOUNT_ID)["marked"])
        self.assertFalse(fixture.row["schedulable"])
        self.assertEqual(fixture.store.snapshot()["recovery_history"], {})

        metadata = fixture.store.snapshot()["scheduler"][str(ACCOUNT_ID)]
        notices = metadata["credit_result_notifications"]
        self.assertEqual(set(notices), {task["episode"]})
        notice = notices[task["episode"]]
        self.assertTrue(notice["degradation_confirmed"])
        self.assertEqual(notice["outcome"], "unrecovered")
        disposition = self.detection.control(ACCOUNT_ID)["disposition"]
        self.assertEqual(disposition["status"], "completed")
        self.assertTrue(disposition["marked"])
        self.assertTrue(disposition["schedule_verified"])
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})

        fixture.controller.deliver_results(notifier)
        delivered_notice = fixture.store.snapshot()["scheduler"][str(ACCOUNT_ID)][
            "credit_result_notifications"
        ][task["episode"]]
        self.assertEqual(delivered_notice["status"], "delivered")
        self.assertEqual(delivered_notice["attempts"], 1)
        self.assertEqual(len(bark_server.requests), 1)
        request = bark_server.requests[0]
        self.assertEqual(request["path"], "/push")
        self.assertEqual(request["headers"].get("Content-Type"), "application/json")
        payload = request["payload"]
        self.assertEqual(payload["device_key"], BARK_DEVICE_KEY)
        self.assertEqual(payload["level"], "critical")
        self.assertEqual(payload["sound"], "alarm")
        self.assertEqual(payload["group"], "oauth-reset-credit")
        self.assertIn("模型验证已判定降智，降智标记与停止调度均已确认", payload["body"])
        self.assertIn("结果：用卡成功，恢复未完成", payload["body"])
        serialized = json.dumps(payload, ensure_ascii=False)
        for private_value in (ADMIN_TOKEN, "fixture-access-v1", OUTPUT):
            self.assertNotIn(private_value, serialized)

        fixture.make_controller()
        self._bind_monitor()
        for _ in range(2):
            await self._card_cycle()
            fixture.controller.deliver_results(notifier)
            self.detection.deliver_notifications()

        self.assertEqual(fixture.calls.count("reset"), 1)
        self.assertEqual(len(self.model_calls), 1)
        self.assertEqual(len(bark_server.requests), 1)
        self.assertEqual(
            fixture.store.snapshot()["scheduler"][str(ACCOUNT_ID)][
                "credit_result_notifications"
            ][task["episode"]]["attempts"],
            1,
        )
        self.assertEqual(self.detection.data().get("disposition_notifications", {}), {})
