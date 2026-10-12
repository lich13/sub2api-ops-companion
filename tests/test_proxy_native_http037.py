"""Exercise the native proxy boundary over an isolated loopback HTTP socket."""
import asyncio
import json
import threading
import unittest
from contextlib import asynccontextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import httpx
from fastapi import HTTPException

from app.proxy_diagnostics import ProxyDiagnostics


class NativeProxyHttpTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.requests = []
        self.entered, self.release = threading.Event(), threading.Event()
        self.release.set()
        self.status = 200
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                fixture.requests.append((self.path, json.loads(body),
                                         self.headers.get('x-api-key') == 'fixture-proxy-admin'))
                fixture.entered.set()
                fixture.release.wait(3)
                result = ({'success': True, 'latency_ms': 13, 'ip_address': '192.0.2.8',
                           'message': 'proxy https://fixture-user:fixture-pass@example.invalid'}
                          if self.path.endswith('/test') else
                          {'score': 80, 'grade': 'yellow', 'checked_at': 1791774000,
                           'items': [{'target': 'OpenAI', 'status': 'challenge',
                                      'http_status': 403, 'latency_ms': 21, 'message': 'challenge'}]})
                payload = json.dumps({'code': 0, 'data': result}).encode()
                self.send_response(fixture.status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

        class Actions:
            @asynccontextmanager
            async def client(self, key, timeout):
                async with httpx.AsyncClient(
                    base_url=f'http://127.0.0.1:{fixture.server.server_port}/api/v1/admin/',
                    headers={'x-api-key': key}, timeout=timeout, follow_redirects=False, trust_env=False,
                ) as client:
                    yield client

        self.diagnostics = ProxyDiagnostics(Actions())

    async def asyncTearDown(self):
        self.release.set()
        await asyncio.to_thread(self.server.shutdown)
        self.server.server_close()
        self.thread.join(2)

    async def test_native_paths_headers_and_whitelist_arrive_at_real_receiver(self):
        connection = await self.diagnostics.run('fixture-proxy-admin', 7, 'test')
        quality = await self.diagnostics.run('fixture-proxy-admin', 7, 'quality-check')
        self.assertEqual(self.requests, [('/api/v1/admin/proxies/7/test', {}, True),
                                         ('/api/v1/admin/proxies/7/quality-check', {}, True)])
        self.assertEqual(connection['latency_ms'], 13)
        self.assertNotIn('fixture-pass', json.dumps(connection))
        self.assertEqual((quality['score'], quality['items'][0]['status']), (80, 'challenge'))

    async def test_inflight_clicks_share_one_actual_request_and_failure_is_not_replayed(self):
        self.release.clear()
        first = asyncio.create_task(self.diagnostics.run('fixture-proxy-admin', 7, 'test'))
        self.assertTrue(await asyncio.to_thread(self.entered.wait, 2))
        second = asyncio.create_task(self.diagnostics.run('fixture-proxy-admin', 7, 'test'))
        await asyncio.sleep(0)
        self.release.set()
        self.assertEqual(await first, await second)
        self.assertEqual(len(self.requests), 1)
        self.status = 503
        with self.assertRaises(HTTPException):
            await self.diagnostics.run('fixture-proxy-admin', 7, 'quality-check')
        self.assertEqual(len(self.requests), 2)
