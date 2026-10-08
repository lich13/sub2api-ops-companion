import asyncio
import copy
import json
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

from app.model_test_stream import TestFailure, execute, failure
from app.model_tests import ModelTests, ModelTestRequest

OUTPUT = ' '.join(str(i * 13 % 355 + 1) for i in range(80))


class ModelConcurrencyTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.row = {'id': 387, 'name': 'fixture', 'platform': 'openai', 'type': 'apikey', 'status': 'active',
                    'schedulable': True, 'parent_account_id': None, 'extra': {}, 'proxy_id': None,
                    'credentials': {'api_key': 'secret-fixture', 'base_url': 'http://not-used.invalid'}}
        db = SimpleNamespace(fetch_one=lambda *args, **kwargs: copy.deepcopy(self.row))
        self.tests = ModelTests(SimpleNamespace(r=SimpleNamespace(db=db, settings=SimpleNamespace(
            usage_query_state_path=str(Path(self.tmp.name) / 'oauth.json'), audit_path=str(Path(self.tmp.name) / 'audit.jsonl'))),
            actions=SimpleNamespace(account=AsyncMock(return_value=self.row), model_allowed=AsyncMock(return_value=True))))
        self.addAsyncCleanup(self.tests.close)

    async def start(self, concurrency=1):
        return await self.tests.start(387, ModelTestRequest(model_id='gpt-6-luna', expected_version='a' * 64,
                                                           request_id=f'test-{time.monotonic_ns()}', concurrency=concurrency))

    async def test_concurrency_one_two_three_peaks_and_no_raw_content_in_summary(self):
        for slots in (1, 2, 3):
            active, peak, count = 0, 0, 0
            async def run(**kwargs):
                nonlocal active, peak, count
                active += 1; peak = max(peak, active); count += 1
                try: await asyncio.sleep(.035); return OUTPUT, 'gpt-6-luna'
                finally: active -= 1
            self.tests.execute = run
            job = await self.start(slots); await self.tests.tasks[job['id']]
            final = self.tests.get(job['id'])
            self.assertEqual((peak, count, active), (slots, 3, 0))
            self.assertEqual((final['completed_groups'], final['valid_groups'], final['status']), (3, 3, 'completed'))
            self.assertEqual(len(final['groups']), 3)
            persisted = self.tests.store.path.read_text()
            for secret in ('secret-fixture', OUTPUT, 'input_text', 'prompt', 'reasoning_effort'):
                self.assertNotIn(secret, persisted)

    async def test_retry_backoff_releases_request_slot(self):
        calls = []
        async def run(**kwargs):
            calls.append(kwargs['prompt'])
            if len(calls) == 1: raise TestFailure('network_error', True)
            return OUTPUT, 'gpt-6-luna'
        self.tests.execute = run
        job = await self.start(1); await self.tests.tasks[job['id']]
        self.assertEqual(len(calls), 4)
        self.assertNotEqual(calls[0], calls[1])
        self.assertEqual(calls[0], calls[3])
        final = self.tests.get(job['id'])
        self.assertEqual(final['groups'][0]['attempts'], 2)

    async def test_cancel_stops_active_streams_and_waiting_retry(self):
        started = asyncio.Event(); cancelled = []
        async def run(**kwargs):
            started.set()
            try: await asyncio.sleep(30)
            except asyncio.CancelledError: cancelled.append(True); raise
        self.tests.execute = run
        job = await self.start(3); await started.wait(); await asyncio.sleep(.05)
        self.assertGreater(self.tests.get(job['id'])['duration_ms'], 0)
        result = await self.tests.cancel(job['id'])
        self.assertEqual(result['status'], 'cancelled')
        self.assertEqual(len(cancelled), 3)
        self.assertFalse(self.tests.tasks)

    async def test_fatal_error_cancels_peers_without_retry(self):
        self.assertFalse(failure(0, {'error': {'message': 'Concurrency limit exceeded for account, please retry later'}}).retryable)
        calls = 0
        async def run(**kwargs):
            nonlocal calls
            calls += 1
            if calls == 1:
                await asyncio.sleep(.04)
                raise TestFailure('rate_limited')
            await asyncio.sleep(30)
        self.tests.execute = run
        job = await self.start(3); await asyncio.wait_for(self.tests.tasks[job['id']], 1)
        final = self.tests.get(job['id'])
        self.assertEqual(final['status'], 'failed')
        self.assertEqual(final['attempts'], 3)
        self.assertEqual(sum(g['status'] == 'cancelled' for g in final['groups']), 2)

    async def test_cancel_drains_pending_progress_write_before_terminal_state(self):
        entered, release = threading.Event(), threading.Event()
        original = self.tests.update
        def delayed(job_id, **changes):
            if 'groups' in changes and not entered.is_set():
                entered.set(); release.wait(3)
            return original(job_id, **changes)
        self.tests.update = delayed
        self.tests.execute = AsyncMock(return_value=(OUTPUT, 'gpt-6-luna'))
        job = await self.start(1)
        try:
            self.assertTrue(await asyncio.to_thread(entered.wait, 1))
            cancellation = asyncio.create_task(self.tests.cancel(job['id']))
            await asyncio.sleep(.025)
            self.assertFalse(cancellation.done())
        finally:
            release.set()
        result = await asyncio.wait_for(cancellation, 2)
        self.assertEqual(result['status'], 'cancelled')
        self.assertTrue(all(g['status'] == 'cancelled' for g in result['groups']))
        self.tests.execute.assert_not_awaited()

    async def test_invalid_completed_sample_is_not_retried_and_idempotency_binds_concurrency(self):
        self.tests.execute = AsyncMock(return_value=('1 1', 'gpt-6-luna'))
        job = await self.start(2); await self.tests.tasks[job['id']]
        self.assertEqual(self.tests.execute.await_count, 3)
        self.assertEqual(self.tests.get(job['id'])['valid_groups'], 0)
        with self.assertRaises(Exception):
            await self.tests.start(387, ModelTestRequest(model_id='gpt-6-luna', expected_version='a' * 64,
                                                       request_id=job['request_id'], concurrency=1))

    async def test_manual_schedule_and_passive_quota_updates_do_not_change_pinned_target(self):
        async def sample(**kwargs):
            self.row['schedulable'] = not self.row['schedulable']
            self.row['extra'] = {'codex_quota': {'used_percent': 42}}
            return OUTPUT, 'gpt-6-luna'
        self.tests.execute = sample
        job = await self.start(1); await self.tests.tasks[job['id']]
        self.assertEqual(self.tests.get(job['id'])['status'], 'completed')
        self.assertEqual(self.tests.get(job['id'])['attempts'], 3)

    async def test_small_completion_frame_returns_without_waiting_for_four_kib_or_eof(self):
        requests = []
        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *args): pass
            def do_POST(self):
                requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Connection', 'close'); self.end_headers()
                self.wfile.write(('data: ' + json.dumps({'type': 'response.completed', 'response': {'status': 'completed',
                    'model': 'gpt-6-luna', 'output': [{'type': 'message', 'content': [{'type': 'output_text', 'text': OUTPUT}]}]}}) + '\n\n').encode()); self.wfile.flush()
                time.sleep(.6)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        self.addCleanup(server.server_close); self.addCleanup(server.shutdown)
        first = AsyncMock()
        start = time.monotonic()
        text, model = await asyncio.wait_for(execute(f'http://127.0.0.1:{server.server_port}/responses', {}, None,
            'gpt-6-luna', 'fixture', 80, oauth=True, on_first_text=first), .5)
        self.assertLess(time.monotonic() - start, .5)
        self.assertEqual((text, model), (OUTPUT, 'gpt-6-luna'))
        self.assertEqual(len(requests), 1); first.assert_awaited_once()
        for field in ('tools', 'reasoning', 'reasoning_effort', 'thinking', 'max_output_tokens'):
            self.assertNotIn(field, requests[0])

    async def test_actual_http_three_samples_obey_requested_peak_and_target(self):
        counts = {'active': 0, 'peak': 0, 'requests': 0}
        bodies, lock = [], threading.Lock()
        round_state = {'concurrency': 0, 'first_batch_ready': threading.Event(), 'barrier_timed_out': False}
        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *args): pass
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                with lock:
                    bodies.append(body); counts['active'] += 1; counts['requests'] += 1
                    counts['peak'] = max(counts['peak'], counts['active'])
                    # Only the first batch waits; the final partial batch must proceed.
                    ready = round_state['first_batch_ready'] if counts['requests'] <= round_state['concurrency'] else None
                    if ready is not None and counts['active'] == round_state['concurrency']:
                        ready.set()
                if ready is not None and not ready.wait(5):
                    with lock:
                        round_state['barrier_timed_out'] = True
                    ready.set()
                payload = ('data: ' + json.dumps({'type': 'response.completed', 'response': {'status': 'completed',
                    'model': 'gpt-6-luna', 'output': [{'type': 'message', 'content': [{'type': 'output_text', 'text': OUTPUT}]}]}}) + '\n\n').encode()
                with lock: counts['active'] -= 1
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(payload))); self.end_headers()
                self.wfile.write(payload); self.wfile.flush()
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close); self.addCleanup(server.shutdown)
        self.row['credentials']['base_url'] = f'http://127.0.0.1:{server.server_port}'
        for concurrency in (1, 2, 3):
            with lock:
                counts.update(active=0, peak=0, requests=0)
                round_state.update(concurrency=concurrency, first_batch_ready=threading.Event(), barrier_timed_out=False)
            job = await self.start(concurrency); await self.tests.tasks[job['id']]
            result = self.tests.get(job['id'])
            self.assertFalse(round_state['barrier_timed_out'],
                f'first HTTP batch did not reach concurrency={concurrency} within 5 seconds; peak={counts["peak"]}')
            self.assertEqual(result['status'], 'completed')
            self.assertEqual((counts['peak'], counts['requests']), (concurrency, 3))
            self.assertTrue(all(group['ttft_ms'] is not None for group in result['groups']))
            print(f'ModelTrace isolated HTTP concurrency={concurrency}; peak={counts["peak"]}; requests={counts["requests"]}')
        self.assertTrue(all(body['model'] == 'gpt-6-luna' and 'reasoning' not in body and 'tools' not in body for body in bodies))
