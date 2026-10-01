import asyncio
import copy
import json
import tempfile
import threading
import unittest
from contextlib import contextmanager
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fastapi import HTTPException
from app.account_locks import AccountLease, account_lock
from app.account_operations import AccountOperations, OperationRequest, busy
from app.desktop_actions import DesktopActions
from app.operation_versions import versions


class Queue023Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.rows = {i: {'id': i, 'name': f'fixture-{i}', 'platform': 'openai', 'type': 'apikey',
            'status': 'active', 'schedulable': True, 'priority': 1, 'extra': {}, 'updated_at': 'old',
            'group_ids': [], 'parent_account_id': None, 'credential_version': 'secret-hash'} for i in (1, 2, 3)}
        self.calls, self.disconnect = [], False
        fixture = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_PUT(self):
                aid = int(self.path.split('/')[-1]); data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                fixture.calls.append((aid, data)); fixture.rows[aid].update(data)
                if fixture.disconnect: self.close_connection = True; return
                self.send_response(200); self.send_header('Content-Type', 'application/json'); self.end_headers()
                self.wfile.write(b'{"code":0,"data":{}}')
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close); self.addCleanup(self.server.shutdown)
        db = SimpleNamespace(fetch_one=lambda sql, params: copy.deepcopy(self.rows.get(params['id'])))
        settings = SimpleNamespace(usage_query_state_path=str(Path(self.tmp.name)/'oauth.json'), audit_path=str(Path(self.tmp.name)/'audit.jsonl'))
        self.s = SimpleNamespace(r=SimpleNamespace(db=db, settings=settings, key_fallback_controller=None,
            oauth_monitor=None, oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: 'admin-fixture'),
            oauth_base_url=lambda: f'http://127.0.0.1:{self.server.server_port}'),
            account_lock=lambda aid: account_lock(db, aid), authenticate=lambda *a, **kw: None, invalidate=lambda: None)
        @contextmanager
        def guard(row):
            lease = AccountLease(db, row, include_account=False)
            if not lease.acquire(): raise busy()
            try: yield
            finally: lease.release()
        self.s.recovery_guard = guard
        self.s.actions = DesktopActions(self.s)
        self.queue = AccountOperations(self.s)
        self.loop = asyncio.create_task(self.queue.loop())
        self.addAsyncCleanup(self.cleanup)

    async def cleanup(self):
        self.loop.cancel(); await asyncio.gather(self.loop, return_exceptions=True); await self.queue.close()

    async def submit(self, aid=1, priority=5, rid='priority-request-0001', version=None):
        return await self.queue.submit(aid, OperationRequest(action='priority', payload={'priority': priority},
            request_id=rid, client_id='fixture-client-0001', expected_version=version or versions(self.rows[aid])['priority']), 'admin-fixture')

    async def final(self, job):
        for _ in range(100):
            result = self.queue.get(job['id'])
            if result['status'] not in {'queued', 'running', 'checking'}: return result
            await asyncio.sleep(.02)
        self.fail('operation did not settle')

    async def test_busy_queues_other_account_runs_then_exact_http_write(self):
        lock = self.s.account_lock(1); lock.acquire()
        try:
            first = await self.submit(); second = await self.submit(2, rid='priority-request-0002')
            self.assertEqual((await self.final(second))['status'], 'completed')
            self.assertEqual(self.queue.get(first['id'])['status'], 'queued')
            self.assertEqual(self.calls, [(2, {'priority': 5})])
        finally: lock.release()
        self.assertEqual((await self.final(first))['status'], 'completed')
        self.assertEqual(self.calls, [(2, {'priority': 5}), (1, {'priority': 5})])
        self.assertEqual(self.queue.store.path.stat().st_mode & 0o777, 0o600)
        self.assertNotIn('admin-fixture', self.queue.store.path.read_text())

    async def test_irrelevant_change_proceeds_real_change_preserves_intent(self):
        lock = self.s.account_lock(1); lock.acquire()
        try:
            first = await self.submit(); self.rows[1]['updated_at'] = 'new'; self.rows[1]['extra'] = {'quota': 42}
        finally: lock.release()
        self.assertEqual((await self.final(first))['status'], 'completed')
        lock.acquire()
        try:
            other = await self.submit(priority=9, rid='priority-request-0003')
            self.rows[1]['priority'] = 7
        finally: lock.release()
        result = await self.final(other)
        self.assertEqual((result['status'], result['requested']['priority'], result['current']['priority']), ('needs_confirmation', 9, 7))
        self.assertEqual(len(self.calls), 1)

    async def test_idempotence_supersession_cancel_and_same_parent(self):
        self.rows[2]['parent_account_id'] = 1
        lock = self.s.account_lock(1); lock.acquire()
        try:
            first = await self.submit(); duplicate = await self.submit()
            self.assertEqual(first['id'], duplicate['id'])
            with self.assertRaises(HTTPException): await self.submit(priority=6)
            next_job = await self.submit(priority=7, rid='priority-request-0004')
            self.assertEqual(self.queue.get(first['id'])['status'], 'superseded')
            child = await self.submit(2, rid='priority-request-0005')
            await asyncio.sleep(.3)
            self.assertEqual(self.calls, [])
            await self.queue.cancel(next_job['id'])
        finally: lock.release()
        self.assertEqual((await self.final(child))['status'], 'completed')
        self.assertEqual(self.calls, [(2, {'priority': 5})])

    async def test_restart_queued_resumes_but_uncertain_write_only_checks(self):
        self.loop.cancel(); await asyncio.gather(self.loop, return_exceptions=True)
        queued = await self.submit()
        self.queue = AccountOperations(self.s)
        await self.queue.run(queued['id'])
        self.assertEqual(self.queue.get(queued['id'])['status'], 'completed')
        uncertain = await self.submit(priority=9, rid='priority-request-0006')
        self.queue.update(uncertain['id'], status='running')
        self.queue = AccountOperations(self.s)
        await self.queue.run(uncertain['id'])
        self.assertEqual(self.queue.get(uncertain['id'])['status'], 'needs_confirmation')
        self.assertEqual(self.calls, [(1, {'priority': 5})])

    async def test_timeout_readback_completes_without_replay_and_save_failure_never_sends(self):
        self.disconnect = True
        job = await self.submit()
        self.assertEqual((await self.final(job))['status'], 'completed')
        self.assertEqual(len(self.calls), 1)
        with patch('app.account_operations.write_json', side_effect=OSError('fixture')):
            with self.assertRaises(OSError): await self.submit(priority=8, rid='priority-request-0007')
        await asyncio.sleep(.3)
        self.assertEqual(len(self.calls), 1)

    async def test_already_satisfied_short_operation_is_only_verified(self):
        job = await self.submit(priority=1, rid='priority-request-0008')
        result = await self.final(job)
        self.assertEqual(result['status'], 'completed')
        self.assertEqual(self.calls, [])
