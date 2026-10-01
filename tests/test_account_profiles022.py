import copy
import json
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import HTTPException
from app.account_locks import AccountLease
from app.account_model_profiles import AccountModelProfiles, ApplyRequest, ProfilesRequest, combine, split
from app.capacity_alerts import CapacityAlertStore, mark_view


class Db:
    def __init__(self):
        self.rows = {
            aid: {'id': aid, 'name': name, 'platform': 'openai', 'type': 'oauth', 'deleted_at': None,
                  'parent_account_id': None, 'passthrough': False, 'model_mapping': mapping,
                  'status': 'active', 'schedulable': False, 'credentials': {'access_token': 'protected-token'}, 'extra': {'automation': True}}
            for aid, name, mapping in [(396, 'tmq', {m: m for m in ('gpt-6-sol', 'gpt-6-astra', 'gpt-6.1-sol')}),
                                      (387, 'yx', {'gpt-6-luna': 'gpt-6-luna', 'gpt-6-astra': 'gpt-6-luna'}), (413, 'surge', {})]}

    def fetch_one(self, sql, params):
        return copy.deepcopy(self.rows.get(params['id']))

    def fetch_all(self, sql):
        return copy.deepcopy(list(self.rows.values()))


class ProfileTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)
        self.db = Db()
        self.alerts = CapacityAlertStore(self.path / 'alerts.json')
        self.service = SimpleNamespace(invalidate=Mock(), r=SimpleNamespace(db=self.db,
            settings=SimpleNamespace(usage_query_state_path=str(self.path / 'oauth.json'), audit_path=str(self.path / 'audit.jsonl')),
            capacity_alerts=SimpleNamespace(store=self.alerts), oauth_base_url=lambda: 'http://mock.invalid',
            oauth_monitor=SimpleNamespace(store=SimpleNamespace(admin_token=lambda: 'local-test-key'))))
        self.p = AccountModelProfiles(self.service)
        self.writes = []
        def write(aid, mapping):
            self.writes.append((aid, dict(mapping)))
            self.db.rows[aid]['model_mapping'] = dict(mapping)
            return 'ok'
        self.p.writer = write

    def mark(self, aid, value):
        old = mark_view(aid, self.alerts.snapshot()['marks'].get(str(aid)))
        return self.alerts.set_mark(aid, value, old['version'], datetime.now(timezone.utc),
                                   intent_factory=self.p.mark_intent_factory(aid))

    def create(self):
        preview = self.p.preview()
        return self.p.apply(ApplyRequest(preview_version=preview['version'], request_id='request-profile-0001'))

    def test_import_is_exact_independent_and_does_not_write_accounts(self):
        imported = self.p.initialize_sources()
        self.assertEqual(imported['normal']['whitelist'], ['gpt-6-sol', 'gpt-6-astra', 'gpt-6.1-sol'])
        self.assertEqual(imported['degraded'], {'whitelist': ['gpt-6-luna'], 'mappings': [{'source': 'gpt-6-astra', 'target': 'gpt-6-luna'}]})
        self.db.rows[396]['model_mapping'] = {}
        self.assertEqual(self.p.view(), imported)
        self.assertEqual(self.writes, [])
        self.assertNotIn('protected-token', json.dumps(self.p.view()))
        self.assertEqual(self.p.store.path.stat().st_mode & 0o777, 0o600)

    def test_native_combination_exact_whitelist_wildcard_source_and_unrestricted(self):
        value = {'whitelist': ['gpt-6-luna'], 'mappings': [{'source': 'gpt-*', 'target': 'gpt-6-luna'}]}
        self.assertEqual(combine(value), {'gpt-6-luna': 'gpt-6-luna', 'gpt-*': 'gpt-6-luna'})
        self.assertEqual(combine(split({})), {})
        for value in ({'whitelist': ['gpt-*']}, {'whitelist': ['a', 'a']}, {'whitelist': ['a'], 'mappings': [{'source': 'a', 'target': 'b'}]},
                      {'mappings': [{'source': 'a*b', 'target': 'b'}]}, {'mappings': [{'source': 'a*', 'target': '*'}]}):
            with self.assertRaises(HTTPException): combine(value)

    def test_preview_includes_key_disabled_excludes_shadow_deleted_and_detects_conflict(self):
        self.p.initialize_sources()
        for aid, change in [(1, {'type': 'apikey'}), (2, {'parent_account_id': 396}), (3, {'deleted_at': '2026-10-01'}),
                            (4, {'status': 'disabled'}), (5, {'passthrough': True}), (6, {'platform': 'grok'})]:
            self.db.rows[aid] = {**self.db.rows[413], 'id': aid, **change}
        items = self.p.preview()['items']
        self.assertEqual({i['account_id'] for i in items}, {396, 387, 413, 1, 4, 5})
        self.assertEqual(next(i for i in items if i['account_id'] == 5)['status'], 'conflict')
        self.assertEqual(next(i for i in items if i['account_id'] == 396)['status'], 'unchanged')

    def test_busy_target_does_not_block_others_and_only_mapping_changes(self):
        self.p.initialize_sources(); job = self.create()
        protected = {aid: copy.deepcopy({k: v for k, v in row.items() if k != 'model_mapping'}) for aid, row in self.db.rows.items()}
        lease = AccountLease(self.db, self.db.rows[387]); self.assertTrue(lease.acquire())
        try:
            for source, item in self.p.pending(): self.p.process(source, item)
            self.assertEqual([aid for aid, _ in self.writes], [413])
        finally: lease.release()
        for source, item in self.p.pending(): self.p.process(source, item)
        self.assertEqual(sorted(aid for aid, _ in self.writes), [387, 413])
        for aid, row in self.db.rows.items(): self.assertEqual({k: v for k, v in row.items() if k != 'model_mapping'}, protected[aid])
        self.assertFalse(any(i['status'] in {'queued', 'writing'} for i in self.p.job(job['id'])['items']))

    def test_mark_outbox_is_atomic_and_latest_mark_wins(self):
        self.p.initialize_sources()
        self.mark(413, True); old = copy.deepcopy(self.alerts.snapshot()['profile_intents']['413'])
        self.mark(413, False)
        self.p.process('mark', old)
        self.assertEqual(self.writes, [])
        for source, item in self.p.pending(): self.p.process(source, item)
        self.assertEqual(self.writes[0][1], combine(self.p.view()['normal']))
        with patch('app.capacity_alerts.write_json', side_effect=OSError('disk full')):
            with self.assertRaises(OSError): self.mark(413, True)
        self.assertFalse(self.alerts.snapshot()['marks']['413']['marked'])

    def test_unconfigured_install_marks_without_automatic_account_changes(self):
        self.mark(413, True)
        self.assertEqual(self.p.pending(), [])
        self.assertTrue(self.alerts.snapshot()['marks']['413']['marked'])

    def test_invalid_existing_mapping_does_not_prevent_bark_suppression(self):
        self.p.initialize_sources()
        self.db.rows[413]['model_mapping'] = {'invalid': False}
        result = self.mark(413, True)
        self.assertTrue(result['marked'])
        self.assertEqual(self.p.mark_status(413)['status'], 'failed')
        self.assertEqual(self.p.pending(), [])
        self.assertEqual(self.writes, [])

    def test_mark_changed_during_dispatched_write_converges_to_latest_template(self):
        self.p.initialize_sources(); self.mark(413, True)
        original = self.p.writer
        def delayed(aid, mapping):
            self.mark(aid, False)
            return original(aid, mapping)
        self.p.writer = delayed
        for source, item in self.p.pending(): self.p.process(source, item)
        self.p.writer = original
        for source, item in self.p.pending(): self.p.process(source, item)
        self.assertFalse(self.alerts.snapshot()['marks']['413']['marked'])
        self.assertEqual(self.db.rows[413]['model_mapping'], combine(self.p.view()['normal']))
        self.assertEqual(len(self.writes), 2)

    def test_template_and_account_conflicts_stop_write_without_losing_mark(self):
        config = self.p.initialize_sources(); self.mark(413, True)
        self.p.save(ProfilesRequest(expected_version=config['version'], normal=config['normal'], degraded={'whitelist': [], 'mappings': []}))
        for source, item in self.p.pending(): self.p.process(source, item)
        self.assertEqual(self.p.mark_status(413)['status'], 'conflict')
        self.assertTrue(self.alerts.snapshot()['marks']['413']['marked'])
        self.assertEqual(self.writes, [])
        job = self.create(); self.db.rows[396]['model_mapping'] = {'external': 'external'}
        item = next(i for i in job['items'] if i['account_id'] == 387)
        self.db.rows[387]['model_mapping'] = {'external': 'external'}
        self.p.process(job['id'], item)
        self.assertEqual(self.p.job(job['id'])['items'][1]['status'], 'conflict')

    def test_timeout_and_restart_only_read_back_never_replay(self):
        self.p.initialize_sources(); self.mark(413, True)
        source, item = self.p.pending()[0]
        self.p._save_item(source, item, status='writing')
        resumed = AccountModelProfiles(self.service); resumed.writer = Mock()
        for src, pending in resumed.pending(): resumed.process(src, pending)
        resumed.writer.assert_not_called()
        self.assertEqual(resumed.mark_status(413)['status'], 'failed')
        self.assertTrue(self.alerts.snapshot()['marks']['413']['marked'])

    def test_actual_bulk_request_contains_only_one_model_mapping_patch(self):
        request = None
        class Response:
            status = 200
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def read(self, limit): return b'{"code":0}'
        def send(value, **kwargs):
            nonlocal request
            request = value; return Response()
        with patch('app.account_model_profiles._urlopen_no_redirect', side_effect=send):
            self.assertEqual(self.p._write_mapping(413, {'a': 'b'}), 'ok')
        self.assertEqual(request.method, 'POST')
        self.assertTrue(request.full_url.endswith('/api/v1/admin/accounts/bulk-update'))
        self.assertEqual(json.loads(request.data), {'account_ids': [413], 'credentials': {'model_mapping': {'a': 'b'}}})

    def test_apply_preview_and_idempotency_conflicts(self):
        self.p.initialize_sources(); preview = self.p.preview()
        request = ApplyRequest(preview_version=preview['version'], request_id='idempotency-test-001')
        one = self.p.apply(request); self.assertEqual(self.p.apply(request)['id'], one['id'])
        self.mark(413, True)
        with self.assertRaises(HTTPException):
            self.p.apply(ApplyRequest(preview_version=preview['version'], request_id='idempotency-test-002'))
