import asyncio
import copy
import json
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from app.codex_identity import codex_identity, codex_originator
from app.model_test_stream import Collector, TestFailure, execute
from app.model_tests import ModelTests
from tests import test_modeltrace022 as fixtures
OUTPUT = fixtures.OUTPUT


class Transport023Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.requests = []
        fixture = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                fixture.requests.append({'headers': dict(self.headers), 'body': json.loads(self.rfile.read(int(self.headers['Content-Length']))), 'path': self.path})
                self.send_response(200)
                self.send_header('Content-Type', fixture.content_type)
                self.send_header('Content-Length', str(len(fixture.body)))
                self.end_headers(); self.wfile.write(fixture.body)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close); self.addCleanup(self.server.shutdown)
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        self.models = ModelTests.__new__(ModelTests)
        self.models.s = SimpleNamespace(actions=SimpleNamespace(model_allowed=AsyncMock(return_value=True)),
            r=SimpleNamespace(db=SimpleNamespace(fetch_all=lambda *args: [
                {'key': 'openai_codex_user_agent', 'value': 'codex_app/0.144.0 (macOS 15.6; arm64) iTerm (codex_app; 0.144.0)'},
                {'key': 'openai_codex_client_version_synced', 'value': '0.159.3'}])))

    async def request(self, kind='apikey', extra=None):
        row = {'id': 421, 'type': kind, 'platform': 'openai', 'extra': extra or {}, 'proxy_id': None,
               'credentials': {'api_key': 'fixture-key', 'access_token': 'fixture-token', 'base_url': self.url}}
        target, _ = await self.models.target(row, 'gpt-6-luna')
        if kind == 'oauth': target['url'] = self.url + '/responses'
        diagnostics = {}
        result = await execute(**target, prompt='fixture challenge', expected=80, diagnostics=diagnostics)
        return result, diagnostics

    async def test_actual_key_and_oauth_headers_and_complete_json(self):
        self.content_type = 'application/json'
        self.body = json.dumps({'object': 'response', 'status': 'completed', 'model': 'returned', 'output_text': OUTPUT}).encode()
        for kind in ('oauth', 'apikey'):
            result, diag = await self.request(kind)
            self.assertEqual(result, (OUTPUT, 'returned'))
            self.assertEqual(diag['end_reason'], 'completed')
            headers = {k.lower(): v for k, v in self.requests[-1]['headers'].items()}
            self.assertEqual(headers['user-agent'], 'codex_app/0.159.3 (macOS 15.6; arm64) iTerm (codex_app; 0.159.3)')
            if kind == 'oauth':
                self.assertEqual((headers['originator'], headers['version']), ('codex_app', '0.159.3'))
            else:
                self.assertFalse({'originator', 'version', 'openai-beta', 'chatgpt-account-id'} & headers.keys())
        self.assertEqual(len(self.requests), 2)
        self.assertNotIn('fixture-key', json.dumps(diag))

    async def test_chat_selection_json_and_sse_end_frames(self):
        self.content_type = 'application/json'
        self.body = json.dumps({'choices': [{'index': 0, 'message': {'content': OUTPUT, 'reasoning_content': 'private'}, 'finish_reason': 'stop'}], 'model': 'returned'}).encode()
        result, diag = await self.request(extra={'openai_responses_supported': False})
        self.assertEqual(result, (OUTPUT, 'returned'))
        self.assertEqual(self.requests[-1]['path'], '/v1/chat/completions')
        self.assertNotIn('reasoning', self.requests[-1]['body'])
        self.assertEqual(diag['response_protocol'], 'chat_completions')
        self.content_type = 'text/event-stream'
        self.body = ('data: '+json.dumps({'choices': [{'index': 0, 'delta': {'content': '1 1 22 '}}], 'model': 'returned'})+'\r\n\r\ndata: [DONE]').encode()
        result, _ = await self.request(extra={'openai_responses_mode': 'force_chat_completions'})
        self.assertEqual(result[0], '1 1 22 ')
        self.assertEqual(len(self.requests), 2)

    async def test_event_name_multiline_crlf_and_final_frame_without_blank(self):
        self.content_type = 'text/event-stream'
        self.body = b'event: response.output_text.delta\r\ndata: {"delta":\r\ndata: "1 1 "}\r\n\r\nevent: response.completed\r\ndata: {"response":{"status":"completed","model":"actual"}}'
        result, diag = await self.request()
        self.assertEqual(result, ('1 1 ', 'actual'))
        self.assertEqual(diag['events'], 2)

    async def test_unknown_is_not_incomplete_and_200_error_is_fatal(self):
        self.content_type = 'application/json'
        for payload, code, retry in [({'success': True}, 'protocol_mismatch', False),
                ({'error': {'message': 'Invalid API key', 'code': 'invalid_api_key'}}, 'auth_or_quota', False)]:
            self.body = json.dumps(payload).encode()
            with self.assertRaises(TestFailure) as ctx: await self.request()
            self.assertEqual((ctx.exception.code, ctx.exception.retryable), (code, retry))
        self.content_type = 'text/event-stream'; self.body = b'data: {"type":"response.output_text.delta","delta":"1 "}\n\n'
        with self.assertRaises(TestFailure) as ctx: await self.request()
        self.assertEqual((ctx.exception.code, ctx.exception.retryable), ('incomplete_stream', True))

    def test_partial_integer_and_final_count_do_not_trigger_stream_guard(self):
        collector = Collector(2)
        collector.accept({'type': 'response.output_text.delta', 'delta': '1 2 3 4 5'})
        self.assertEqual(collector.text, '1 2 3 4 5')
        with self.assertRaises(TestFailure): collector.accept({'type': 'response.output_text.delta', 'delta': ' '})
        collector = Collector(2)
        collector.accept({'object': 'response', 'status': 'completed', 'output_text': '1 2 3 4 5 6'}, whole=True)
        self.assertTrue(collector.completed)
        self.assertEqual(collector.text, '1 2 3 4 5 6')

    def test_native_identity_pairing_and_version_validation(self):
        version, ua = codex_identity({'openai_codex_user_agent': 'bad/1.0 (Linux; x86_64) term (codex-tui; 0.1.0)', 'openai_codex_client_version': '0.160.1-beta.2'})
        self.assertEqual(version, '0.160.1-beta.2')
        self.assertEqual(codex_originator(ua), 'codex-tui')
        self.assertIn('term (codex-tui; 0.160.1-beta.2)', ua)
        self.assertNotIn('bad/', ua)


class Execution023Tests(unittest.IsolatedAsyncioTestCase):
    asyncSetUp = fixtures.ModelConcurrencyTests.asyncSetUp
    start = fixtures.ModelConcurrencyTests.start

    async def test_conflicted_task_can_be_cancelled_without_another_request(self):
        self.tests.execute = AsyncMock(return_value=(OUTPUT, 'gpt-6-luna'))
        job = await self.start(1)
        await self.tests.tasks[job['id']]
        self.tests.tasks.pop(job['id'], None)
        self.tests.update(job['id'], status='needs_confirmation')
        count = self.tests.execute.await_count
        result = await self.tests.cancel(job['id'])
        self.assertEqual(result['status'], 'cancelled')
        self.assertEqual(self.tests.execute.await_count, count)

    async def test_99_boundary_uses_unrounded_value(self):
        for probability, expected in ((.989999, 3), (.99, 1)):
            self.tests.execute = AsyncMock(return_value=(OUTPUT, 'gpt-6-luna'))
            with patch('app.model_tests.analyze', return_value={'prediction_name': 'fixture', 'probability': probability, 'used_outputs': 1}):
                job = await self.start(1); await self.tests.tasks[job['id']]
            self.assertEqual(self.tests.execute.await_count, expected)
            result = self.tests.get(job['id'])
            self.assertEqual(result['completion_reason'], 'confidence_99' if expected == 1 else 'samples_finished')

    async def test_early_finish_cancels_other_active_streams(self):
        calls, cancelled = 0, 0
        async def run(**kwargs):
            nonlocal calls, cancelled
            calls += 1
            if calls == 1:
                await asyncio.sleep(.04); return OUTPUT, 'gpt-6-luna'
            try: await asyncio.sleep(30)
            except asyncio.CancelledError: cancelled += 1; raise
        self.tests.execute = run
        with patch('app.model_tests.analyze', return_value={'probability': .99, 'used_outputs': 1}):
            job = await self.start(3); await asyncio.wait_for(self.tests.tasks[job['id']], 1)
        self.assertEqual((calls, cancelled), (3, 2))
        self.assertEqual(self.tests.get(job['id'])['valid_groups'], 1)

    async def test_retryable_group_isolated_and_successful_groups_not_resent(self):
        first, calls, fail = None, [], True
        async def run(**kwargs):
            nonlocal first
            first = first or kwargs['prompt']; calls.append(kwargs['prompt'])
            if fail and kwargs['prompt'] == first: raise TestFailure('incomplete_stream', True)
            return OUTPUT, 'gpt-6-luna'
        self.tests.execute = run
        with patch('app.model_tests.analyze', return_value={'probability': .5, 'used_outputs': 2}):
            job = await self.start(3); await self.tests.tasks[job['id']]
            result = self.tests.get(job['id'])
            self.assertEqual((len(calls), result['completed_groups'], result['can_retry']), (5, 2, True))
            fail = False
            await self.tests.retry_failed(job['id'], 'retry-request-00001'); await self.tests.tasks[job['id']]
            self.assertEqual(len(calls), 6)
            self.assertEqual(calls[-1], first)
            self.assertEqual(self.tests.get(job['id'])['completed_groups'], 3)
            await self.tests.retry_failed(job['id'], 'retry-request-00001')
            self.assertEqual(len(calls), 6)

    async def test_global_two_jobs_and_same_credential_scope_wait(self):
        original = copy.deepcopy(self.row)
        def read(sql, params):
            row = copy.deepcopy(original); row['id'] = params['id']; return row
        self.tests.s.r.db.fetch_one = read
        active, peak = 0, 0; second_started = asyncio.Event(); release = asyncio.Event()
        async def run(**kwargs):
            nonlocal active, peak
            active += 1; peak = max(peak, active)
            if active == 2: second_started.set()
            try: await release.wait(); return OUTPUT, 'model'
            finally: active -= 1
        self.tests.execute = run
        from app.model_tests import ModelTestRequest
        jobs = [await self.tests.start(aid, ModelTestRequest(model_id='gpt-6-luna', expected_version='a'*64, request_id=f'account-request-{aid}')) for aid in (1, 2, 3)]
        await asyncio.wait_for(second_started.wait(), 2)
        self.assertEqual((active, peak), (2, 2))
        self.assertEqual(sum(self.tests.get(j['id'])['status'] == 'queued' for j in jobs), 1)
        release.set(); await asyncio.gather(*list(self.tests.tasks.values()))
        self.assertEqual(peak, 2)
