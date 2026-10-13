"""Isolated HTTP adapter around the production connection-batch implementation."""
from __future__ import annotations

import asyncio
import os
from contextlib import asynccontextmanager, suppress
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urlsplit

import uvicorn
from fastapi import FastAPI, Header, HTTPException

from app.db import Database
from app.desktop_actions import DesktopActions
from app.desktop_api import DesktopService
from app.account_operations import TestBatchRequest
from app.operation_versions import versions


def isolated_config():
    if os.getenv('SUB2OPS_PROBE_ISOLATED') != 'fixture-only':
        raise RuntimeError('Explicit isolated fixture mode is required')
    database = os.environ['PROBE_DATABASE_URL']
    upstream = os.environ['PROBE_UPSTREAM'].rstrip('/')
    for value in (database, upstream):
        parsed = urlsplit(value)
        if parsed.hostname not in {'127.0.0.1', 'localhost', '::1'} or parsed.query or parsed.fragment:
            raise RuntimeError('Only loopback fixture endpoints are accepted')
    if not urlsplit(database).path.startswith('/probe_fixture_'):
        raise RuntimeError('A disposable probe_fixture_ database is required')
    state = Path(os.environ['PROBE_STATE_DIR']).resolve()
    if state == Path('/data') or state == Path.cwd() or not state.is_absolute():
        raise RuntimeError('A separate fixture state directory is required')
    state.mkdir(parents=True, exist_ok=True)
    return database, upstream, state


class FixtureProjection(Database):
    """Both implementations read exactly the same synthetic account records."""
    def fetch_all(self, _sql, params=None):
        params = params or {}
        sql = 'SELECT row_data FROM probe_accounts'
        if 'id' in params:
            sql += ' WHERE id=%(id)s'
        elif 'ids' in params:
            sql += ' WHERE id=ANY(%(ids)s)'
        sql += ' ORDER BY id'
        return [item['row_data'] for item in super().fetch_all(sql, params)
                if not item['row_data'].get('deleted_at')]

    def fetch_one(self, sql, params=None):
        rows = self.fetch_all(sql, params)
        return rows[0] if rows else None


class FixtureActions(DesktopActions):
    def client(self, key, timeout=30):
        return super().client(key, min(timeout, float(os.getenv('PROBE_TIMEOUT_SECONDS', '90'))))


def create_app():
    database, upstream, state = isolated_config()
    db = FixtureProjection(database)
    settings = SimpleNamespace(usage_query_state_path=str(state / 'fixture-state.json'),
        audit_path=str(state / 'audit.jsonl'),
        oauth_recovery_test_model_id=os.getenv('PROBE_MODEL', 'fixture-probe-model'),
        oauth_recovery_test_concurrency=max(1, min(8, int(os.getenv('PROBE_CONCURRENCY', '2')))))
    key = os.getenv('PROBE_ADMIN_KEY', 'fixture-admin-connection-039')
    runtime = SimpleNamespace(db=db, settings=settings, key_fallback_controller=None,
        oauth_base_url=lambda: upstream, oauth_state_store=lambda: SimpleNamespace(admin_token=lambda: key))
    service = DesktopService(runtime)
    service.actions = FixtureActions(service)

    @asynccontextmanager
    async def lifespan(_app):
        db.open()
        task = asyncio.create_task(service.operations.loop())
        try:
            yield
        finally:
            task.cancel()
            with suppress(asyncio.CancelledError):
                await task
            await service.operations.close()
            await service.actions.close()
            db.close()

    app = FastAPI(lifespan=lifespan)

    async def auth(key):
        await asyncio.to_thread(service.authenticate, key or '')

    @app.get('/healthz')
    async def health():
        def check():
            with db.connection() as connection:
                connection.execute('SELECT 1')
        await asyncio.to_thread(check)
        return {'status': 'ok', 'isolation': 'fixture-only'}

    @app.get('/api/desktop/v1/accounts')
    async def accounts(x_api_key: str = Header(default='')):
        await auth(x_api_key)
        rows = await asyncio.to_thread(db.fetch_all, '')
        return {'items': [{**{k: row.get(k) for k in ('id', 'name', 'platform', 'type')},
                           'operation_versions': {'test': versions(row)['test']}} for row in rows]}

    @app.post('/api/desktop/v1/account-operations/batch', status_code=202)
    async def submit(request: TestBatchRequest, x_api_key: str = Header(default='')):
        await auth(x_api_key)
        return await service.operations.submit_batch(request, x_api_key)

    @app.get('/api/desktop/v1/account-operations')
    async def listing(batch_id: str, x_api_key: str = Header(default='')):
        await auth(x_api_key)
        return service.operations.listing(batch_id)

    @app.post('/api/desktop/v1/account-operations/batches/{batch_id}/cancel')
    async def cancel(batch_id: str, x_api_key: str = Header(default='')):
        await auth(x_api_key)
        return await service.operations.cancel_batch(batch_id)

    @app.exception_handler(OSError)
    async def store_error(_request, _error):
        from fastapi.responses import JSONResponse
        return JSONResponse(status_code=503, content={'detail': '操作状态无法保存'})

    return app


if __name__ == '__main__':
    uvicorn.run(create_app(), host='127.0.0.1', port=int(os.getenv('PROBE_PORT', '18191')),
                access_log=False, log_level='error')
