import asyncio
import json
import tempfile
import time
import unittest
from datetime import timedelta
from pathlib import Path
from types import SimpleNamespace

from app.bark import BarkNotifier
from app.capacity_alerts import CapacityAlerts, mark_view, match_message
from app.desktop_api import ACCOUNT_SQL, ERROR_WHERE, error_dto
from app.error_evidence import MESSAGES
from test_capacity_alerts import BarkCapture, CapacityDb, Clock, error_row


def gateway(record_id, at, aid=1, message=MESSAGES[0], **changes):
    return error_row(record_id, account_id=aid, created_at=at, upstream_error_message=None,
        error_message=message, error_owner='platform', error_phase='internal', error_source='gateway', stream=True,
        error_body=json.dumps({'error': {'code': 'server_is_overloaded', 'message': message, 'type': 'service_unavailable_error'}}), **changes)


class GatewayAlertTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.clock = Clock(); self.capture = BarkCapture(); self.addCleanup(self.capture.close)
        self.settings = SimpleNamespace(usage_query_state_path=str(Path(self.tmp.name) / 'quota.json'),
            audit_path=str(Path(self.tmp.name) / 'audit.jsonl'), bark_enabled=True, bark_config_valid=True,
            bark_device_key='isolated-only', bark_server_url=self.capture.url)
        self.db = CapacityDb([], {1: {'platform': 'openai', 'type': 'oauth', 'deleted_at': None},
                                 2: {'platform': 'openai', 'type': 'oauth', 'deleted_at': None}})
        self.alerts = CapacityAlerts(self.settings, self.db, BarkNotifier(self.settings), clock=self.clock)

    def test_gateway_shapes_and_complete_phrases_without_request_body_false_positive(self):
        for message in MESSAGES:
            row = gateway(15884, self.clock(), message='  ' + message.upper().replace(' ', ' \n ') + '!!! ')
            self.assertEqual(match_message(row), message)
            row['error_message'] = None
            self.assertEqual(match_message(row), message)
        self.assertEqual(match_message({**gateway(15884, self.clock()), 'stream': False}), MESSAGES[0])
        for event_type in ('response.failed', 'response.incomplete'):
            row = gateway(15886, self.clock())
            row.update(error_message=None, error_body=json.dumps({'type': event_type, 'response': {'error': {'message': MESSAGES[0]}}}))
            self.assertEqual(match_message(row), MESSAGES[0])
        for change in ({'account_type': 'apikey'}, {'account_platform': 'grok'}, {'error_source': 'client'}, {'stream': False, 'error_body': None},
                       {'account_deleted_at': self.clock()}, {'error_message': 'Selected model is at capacity.', 'error_body': None},
                       {'error_message': 'server_error', 'error_body': '{"error":{"code":"server_error","message":"An error occurred"}}'},
                       {'error_message': None, 'error_body': json.dumps({'request': {'error': {'message': MESSAGES[0]}}})}):
            row = gateway(15888, self.clock()); row.update(change)
            self.assertIsNone(match_message(row), change)

    def test_sql_template_can_be_formatted_and_preserves_provider_history(self):
        sql = ACCOUNT_SQL.format(filter='AND a.id=%(id)s')
        self.assertIn("e.error_owner='platform'", sql)
        self.assertIn("e.error_phase IN ('upstream', 'account_auth')", ERROR_WHERE)
        self.assertIn('EXISTS (SELECT 1 FROM accounts evidence_account', sql)
        record = error_dto({'id': 15886, 'account_id': 387, 'created_at': self.clock(), 'resolved': True,
                            'error_message': MESSAGES[0], 'account_name': None})
        self.assertEqual((record['id'], record['resolved'], record['message']), (15886, True, MESSAGES[0]))

    def test_upgrade_history_visible_but_not_backfilled_marked_still_suppressed(self):
        old = self.clock() - timedelta(seconds=30)
        with self.alerts.store.transaction() as data:
            data.update(cursor=15883, initialized_at=(old - timedelta(days=1)).isoformat())
        self.db.rows.extend([gateway(15884, old), gateway(15886, old, aid=2)])
        self.alerts.poll(); self.alerts.deliver_due()
        self.assertEqual(self.capture.requests, [])
        self.alerts.store.set_mark(2, True, mark_view(2)['version'], self.clock())
        self.clock.advance(seconds=1)
        self.db.rows.extend([gateway(15887, self.clock(), aid=2), gateway(15889, self.clock())])
        self.alerts.poll(); self.alerts.deliver_due()
        self.assertEqual(len(self.capture.requests), 1)
        statuses = self.alerts.store.snapshot()['notifications']
        self.assertEqual(statuses['15887']['reason'], 'degradation_mark')
        self.assertEqual(statuses['15889']['status'], 'delivered')
        self.assertNotIn('15886', statuses)
        self.alerts.poll(); self.alerts.deliver_due()
        self.assertEqual(len(self.capture.requests), 1)

    def test_gateway_first_actual_bark_http_under_three_seconds_while_other_work_waits(self):
        async def scenario():
            collector = asyncio.create_task(self.alerts.collect_loop())
            sender = asyncio.create_task(self.alerts.delivery_loop())
            unrelated_long_work = asyncio.create_task(asyncio.sleep(30))
            try:
                self.assertTrue(await asyncio.to_thread(self.db.initialized.wait, 1))
                self.clock.advance(seconds=1)
                self.db.rows.append(gateway(15884, self.clock()))
                began = time.monotonic()
                self.assertTrue(await asyncio.to_thread(self.capture.wait_for_request, 3))
                latency = time.monotonic() - began
                self.assertLess(latency, 3)
                request = self.capture.requests[0]
                self.assertEqual((request['level'], request['sound'], request['group']), ('critical', 'alarm', 'Sub2Ops 疑似降智'))
                self.assertIn('#15884', request['body'])
                self.assertFalse(unrelated_long_work.done())
                print(f'gateway isolated Bark first HTTP: {latency:.3f}s; requests={len(self.capture.requests)}')
            finally:
                for task in (collector, sender, unrelated_long_work): task.cancel()
                await asyncio.gather(collector, sender, unrelated_long_work, return_exceptions=True)
        asyncio.run(scenario())
