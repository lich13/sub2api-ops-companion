import copy
import unittest
from tests import test_capacity_alerts as alerts_fixture
from tests import test_account_templates027 as templates_fixture
from app.capacity_alerts import CapacityAlerts, MESSAGES, mark_view, match_message


class KeyAlerts023Tests(unittest.TestCase):
    setUp = alerts_fixture.CapacityAlertTests.setUp
    tearDown = alerts_fixture.CapacityAlertTests.tearDown
    make_alerts = alerts_fixture.CapacityAlertTests.make_alerts

    def test_key_upgrade_watermark_mark_suppression_and_detection_clues(self):
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
        self.assertEqual(self.capture.requests, [])
        self.assertEqual(set(alerts.store.snapshot()['detection_events']), {'error:2', 'error:3', 'error:4'})
        alerts.store.set_mark(1, True, mark_view(1)['version'], self.clock())
        self.clock.advance(seconds=1)
        db.rows.append(alerts_fixture.error_row(5, account_type='apikey', created_at=self.clock()))
        alerts.poll()
        resumed = CapacityAlerts(self.settings, db, self.notifier, clock=self.clock)
        resumed.deliver_due()
        self.assertEqual(self.capture.requests, [])
        self.assertNotIn('error:5', resumed.store.snapshot()['detection_events'])
        self.assertTrue(resumed.store.snapshot()['marks']['1']['marked'])
        self.assertEqual(match_message(db.rows[-1]), MESSAGES[0])
        self.assertEqual(resumed.store.snapshot()['notifications']['5']['status'], 'suppressed')


class KeyProfiles023Tests(unittest.TestCase):
    def test_key_template_applies_only_model_mapping(self):
        fixture = templates_fixture.AccountTemplateTests()
        fixture.setUp()
        self.addCleanup(fixture.tearDown)
        row = {
            "id": 501, "name": "key-account", "platform": "openai", "type": "apikey",
            "deleted_at": None, "parent_account_id": None, "passthrough": False,
            "model_mapping": {}, "credentials": {"api_key": "fixture-key", "base_url": "https://gateway.invalid"},
            "proxy_id": 9, "group_ids": [13], "extra": {"pool": {"size": 4}},
            "status": "disabled", "schedulable": False,
        }
        fixture.db.rows[501] = row
        templates = fixture.templates
        view = templates.initialize_sources(1, 2)
        before = copy.deepcopy(row)
        templates.writer = lambda aid, mapping, _key: (fixture.db.rows[aid].update(model_mapping=dict(mapping)) or "ok")
        current = templates.account(501)
        expected = templates_fixture.AccountTemplateTests
        from app.account_templates import TemplateApplication
        from app.account_templates import account_version
        payload = TemplateApplication(expected_version=account_version(current), template_id="degraded", template_version=view["version"])
        result = templates.apply(501, payload, "admin-key")
        self.assertTrue(result["verified"])
        self.assertEqual(row["model_mapping"], {"fixture-model": "fixture-model", "fixture-input-*": "fixture-model", "fixture-terra-*": "fixture-model"})
        self.assertEqual({k: v for k, v in row.items() if k != "model_mapping"}, {k: v for k, v in before.items() if k != "model_mapping"})
