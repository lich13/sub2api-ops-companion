import copy
import unittest
from tests import test_capacity_alerts as alerts_fixture
from tests import test_account_profiles022 as profiles_fixture
from app.capacity_alerts import CapacityAlerts, MESSAGES, mark_view, match_message


class KeyAlerts023Tests(unittest.TestCase):
    setUp = alerts_fixture.CapacityAlertTests.setUp
    tearDown = alerts_fixture.CapacityAlertTests.tearDown
    make_alerts = alerts_fixture.CapacityAlertTests.make_alerts

    def test_key_upgrade_watermark_mark_suppression_and_actual_bark_payload(self):
        self.accounts[1]['type'] = 'apikey'
        alerts, db = self.make_alerts()
        alerts.poll()
        old = self.clock(); self.clock.advance(seconds=1)
        # Simulate upgrade from an existing OAuth-only state, keeping its watermark.
        with alerts.store.transaction() as state: state.pop('apikey_since', None)
        db.rows.append(alerts_fixture.error_row(1, account_type='apikey', created_at=old))
        alerts.poll(); alerts.deliver_due()
        self.assertEqual(self.capture.requests, [])
        for index, message in enumerate(MESSAGES, 2):
            self.clock.advance(seconds=1)
            db.rows.append(alerts_fixture.error_row(index, account_type='apikey', message=message, created_at=self.clock()))
        alerts.poll(); alerts.deliver_due(); alerts.deliver_due(); alerts.deliver_due()
        self.assertEqual(len(self.capture.requests), 3)
        for request in self.capture.requests:
            self.assertEqual(request['title'], '⚠️ Codex 疑似降智')
            self.assertIn('（Key）', request['body'])
            self.assertEqual((request['level'], request['sound']), ('critical', 'alarm'))
        self.clock.advance(seconds=1)
        db.rows.append(alerts_fixture.error_row(5, account_type='apikey', created_at=self.clock()))
        alerts.poll()
        alerts.store.set_mark(1, True, mark_view(1)['version'], self.clock())
        resumed = CapacityAlerts(self.settings, db, self.notifier, clock=self.clock)
        resumed.deliver_due()
        self.assertEqual(len(self.capture.requests), 3)
        self.assertTrue(resumed.store.snapshot()['marks']['1']['marked'])
        self.assertEqual(match_message(db.rows[-1]), MESSAGES[0])
        self.assertEqual(resumed.store.snapshot()['notifications']['5']['status'], 'suppressed')


class KeyProfiles023Tests(unittest.TestCase):
    setUp = profiles_fixture.ProfileTests.setUp
    mark = profiles_fixture.ProfileTests.mark
    create = profiles_fixture.ProfileTests.create

    def test_key_mark_applies_only_mapping_preserves_key_url_proxy_pool(self):
        row = self.db.rows[413]
        row.update(type='apikey', credentials={'api_key': 'private-key', 'base_url': 'https://gateway.invalid'},
                   proxy_id=9, group_ids=[13], extra={'pool': {'size': 4}}, status='disabled', schedulable=False)
        before = copy.deepcopy(row)
        self.p.initialize_sources()
        self.assertEqual(self.writes, [])
        self.mark(413, True)
        for source, item in self.p.pending(): self.p.process(source, item)
        self.assertEqual(row['model_mapping'], self.db.rows[387]['model_mapping'])
        self.assertEqual({k: v for k, v in row.items() if k != 'model_mapping'}, {k: v for k, v in before.items() if k != 'model_mapping'})
        self.mark(413, False)
        for source, item in self.p.pending(): self.p.process(source, item)
        self.assertEqual(row['model_mapping'], self.db.rows[396]['model_mapping'])
        self.assertEqual(len(self.writes), 2)
