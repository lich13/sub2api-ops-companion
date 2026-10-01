import copy
import tempfile
import threading
import unittest
from datetime import timedelta
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from app.account_locks import AccountLease
from app.desktop_api import DesktopService, ScheduleRequest, account_dto
from app.oauth_monitor import OAuthStateStore, build_monitor_candidates
import test_oauth_monitor as baseline
from test_oauth_monitor import NOW, RUN, account, result, summary


class ManualControlTests(unittest.TestCase):
    def setup_monitor(self, directory):
        return baseline.OAuthMonitorExecutionTests().make_monitor(Path(directory), summary(seven_used=100, seven_reset=NOW), summary())

    def service(self, monitor, directory):
        rows = monitor.db.rows
        def read(_sql, params=None):
            return copy.deepcopy(next((r for r in rows if r['id'] == (params or {}).get('id', (params or {}).get('account_id'))), None))
        monitor.db.fetch_one = read
        service = DesktopService.__new__(DesktopService)
        service.r = SimpleNamespace(db=monitor.db, oauth_monitor=monitor, oauth_base_url=lambda: 'http://unused.invalid',
            settings=SimpleNamespace(audit_path=str(Path(directory) / 'audit.jsonl')),
            key_fallback_controller=SimpleNamespace(_lock=threading.Lock(), load_config=lambda: SimpleNamespace(valid=True, managed_account_ids=[])))
        service.config = SimpleNamespace(thread_lock=threading.Lock())
        service.invalidate = Mock()
        def write(aid, enabled, **kwargs):
            next(r for r in rows if r['id'] == aid)['schedulable'] = enabled
            return {'success': True}
        return service, write

    def test_schedule_ignores_long_operation_and_monitor_locks_without_testing(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.setup_monitor(directory)
            monitor.db.rows.append(account(2))
            service, write = self.service(monitor, directory)
            lease = AccountLease(monitor.db, monitor.db.rows[0]); self.assertTrue(lease.acquire())
            monitor._run_lock.acquire()
            service.r.key_fallback_controller._lock.acquire()
            try:
                with patch('app.desktop_api.execute_sub2api_set_schedulable', side_effect=write):
                    for row in monitor.db.rows:
                        payload = ScheduleRequest(schedulable=False, expected_version=account_dto(row, NOW, set())['version'])
                        answer = service.set_schedulable(row['id'], payload, 'fake-admin')
                        self.assertTrue(answer['verified'])
                        self.assertEqual(monitor.store.control_generation(row['id']), 1)
                self.assertEqual((calls['usage'], calls['test'], calls['recovery']), (0, 0, 0))
            finally:
                service.r.key_fallback_controller._lock.release()
                monitor._run_lock.release(); lease.release()

    def test_manual_open_does_not_revive_old_task_across_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.setup_monitor(directory)
            row = monitor.db.rows[0]
            monitor.store.commit(scheduler_updates={1: {'recovery_intent': {'status': 'retry', 'fingerprint': 'old'}}})
            monitor.store.manual_control(row, True, RUN)
            monitor.run_once(RUN + timedelta(seconds=1))
            self.assertEqual((calls['usage'], calls['test']), (0, 0))
            reloaded = OAuthStateStore(str(monitor.store.path))
            candidates = build_monitor_candidates([row], reloaded.results(), reloaded.scheduler(), RUN + timedelta(minutes=2))
            self.assertEqual(candidates, [])
            row['rate_limited_at'] = (RUN + timedelta(seconds=30)).isoformat()
            row['rate_limit_reset_at'] = (RUN + timedelta(seconds=60)).isoformat()
            monitor.run_once(RUN + timedelta(minutes=3))
            self.assertEqual(calls['test'], 1)

    def test_close_during_actual_scheduled_test_discards_late_result(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, calls = self.setup_monitor(directory)
            entered, release = threading.Event(), threading.Event()
            def test(*args, **kwargs):
                entered.set(); release.wait(5)
                return {'success': True}
            monitor.test_runner = test
            thread = threading.Thread(target=lambda: monitor.run_once(RUN)); thread.start()
            self.assertTrue(entered.wait(3))
            service, write = self.service(monitor, directory)
            row = monitor.db.rows[0]
            payload = ScheduleRequest(schedulable=False, expected_version=account_dto(row, NOW, set())['version'])
            try:
                with patch('app.desktop_api.execute_sub2api_set_schedulable', side_effect=write):
                    self.assertTrue(service.set_schedulable(1, payload, 'fake')['verified'])
            finally:
                release.set(); thread.join(5)
            self.assertFalse(row['schedulable'])
            self.assertEqual(calls['recovery'], 0)
            self.assertEqual(monitor.store.snapshot()['recovery_history'], {})
            self.assertEqual(monitor.store.pending_events(), [])

    def test_fresh_independent_evidence_allows_recovery_but_old_evidence_does_not(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, _ = self.setup_monitor(directory)
            row = monitor.db.rows[0]
            monitor.store.manual_control(row, True, RUN)
            monitor.store.commit(results={1: result(summary(), RUN + timedelta(seconds=1))})
            monitor._refresh_inventory(RUN + timedelta(seconds=2), force=True)
            monitor._discover_recoveries([row], RUN + timedelta(seconds=2))
            self.assertTrue(build_monitor_candidates([row], monitor.store.results(), monitor.store.scheduler(), RUN + timedelta(seconds=2)))

    def test_generation_filter_preserves_budget_and_rejects_stale_history(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, _ = self.setup_monitor(directory)
            monitor.store.commit(scheduler_updates={1: {'quota_query': {'automatic_attempts': [NOW.isoformat()]}}})
            monitor.store.manual_control(monitor.db.rows[0], False, RUN)
            monitor.store.commit(scheduler_updates={1: {'recovery_intent': {'status': 'recovered'}}},
                pending_events={'late': {'account_id': 1}}, recovery_history={'late': {'account_id': 1}}, expected_generations={1: 0})
            state = monitor.store.snapshot()
            self.assertEqual(state['scheduler']['1']['quota_query']['automatic_attempts'], [NOW.isoformat()])
            self.assertEqual(state['pending_events'], {})
            self.assertEqual(state['recovery_history'], {})

    def test_failed_control_persistence_never_sends_schedule_request(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor, _ = self.setup_monitor(directory)
            service, _ = self.service(monitor, directory)
            payload = ScheduleRequest(schedulable=False, expected_version=account_dto(monitor.db.rows[0], NOW, set())['version'])
            with patch.object(monitor.store, '_write', side_effect=OSError('disk full')), patch('app.desktop_api.execute_sub2api_set_schedulable') as writer:
                with self.assertRaises(Exception):
                    service.set_schedulable(1, payload, 'fake')
                writer.assert_not_called()
