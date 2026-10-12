"""Durable native bulk writes. An uncertain dispatch is only ever read back."""
from __future__ import annotations

import asyncio
import copy
import uuid
from collections import defaultdict
from contextlib import ExitStack

import httpx
from fastapi import HTTPException
from pydantic import ValidationError

from .account_locks import account_lock
from .operation_versions import digest


def result_ids(data, expected: set[int], action: str):
    """Require an exhaustive, disjoint native result, including its row results."""
    if not isinstance(data, dict):
        return None
    success, failed = data.get('success_ids'), data.get('failed_ids')
    if not isinstance(success, list) or not isinstance(failed, list):
        return None
    ids = success + failed
    if (any(type(i) is not int or i < 1 for i in ids) or len(ids) != len(set(ids))
            or set(ids) != expected or type(data.get('success')) is not int
            or type(data.get('failed')) is not int
            or data['success'] != len(success) or data['failed'] != len(failed)):
        return None
    if 'total' in data and (type(data['total']) is not int or data['total'] != len(expected)):
        return None
    entries = data.get('errors' if action == 'delete' else 'results')
    if not isinstance(entries, list):
        return None
    seen = set()
    for entry in entries:
        if not isinstance(entry, dict) or type(entry.get('account_id')) is not int:
            return None
        aid = entry['account_id']
        if aid in seen or aid not in expected:
            return None
        seen.add(aid)
        if action != 'delete' and (type(entry.get('success')) is not bool
                                  or entry['success'] != (aid in success)):
            return None
    if seen != (set(failed) if action == 'delete' else expected):
        return None
    return set(success), set(failed)


def unsupported(response):
    # A business-level missing account/permission is not a missing route.
    if response.status_code not in {404, 405}:
        return False
    text = response.text.strip().lower()
    if text in {'404 page not found', 'method not allowed', '405 method not allowed'}:
        return True
    try:
        body = response.json()
    except ValueError:
        return False
    return isinstance(body, dict) and body.get('detail') in {'Not Found', 'Method Not Allowed'}


class NativeBatches:
    def __init__(self, queue):
        self.q, self.s = queue, queue.s
        self.locks = {}

    @staticmethod
    def handles(job):
        return bool(job.get('batch_id') and job.get('action') in
                    {'proxy', 'account_template', 'delete', 'groups'}
                    and job.get('status') in {'queued', 'checking'})

    def launch(self, bid):
        task = asyncio.create_task(self.run(bid))
        self.q.tasks[bid] = task
        def done(future):
            self.q.tasks.pop(bid, None)
            if not future.cancelled():
                future.exception()
        task.add_done_callback(done)

    async def rows(self, jobs):
        ids = [j['account_id'] for j in jobs]
        if jobs[0]['action'] == 'account_template':
            return await asyncio.to_thread(self.s.account_templates.accounts, ids)
        return await self.s.actions.accounts(ids)

    async def children(self, ids):
        rows = await asyncio.to_thread(self.s.r.db.fetch_all,
            "SELECT a.id,nullif(to_jsonb(a)->>'parent_account_id','')::bigint AS parent_account_id FROM accounts a "
            "WHERE a.deleted_at IS NULL AND nullif(to_jsonb(a)->>'parent_account_id','')::bigint=ANY(%(ids)s)", {'ids': ids})
        result = {aid: [] for aid in ids}
        for row in rows:
            if row.get('parent_account_id') in result:
                result[row['parent_account_id']].append(row['id'])
        return {aid: sorted(children) for aid, children in result.items()}

    async def submit(self, request, key):
        from .account_operations import stamp
        ids = [item.account_id for item in request.accounts]
        if len(set(ids)) != len(ids):
            raise HTTPException(422, '批量账号不能重复')
        if request.payload != ({'confirmed': True} if request.action == 'delete' else {}):
            raise HTTPException(422, '请确认删除，或核对批量操作参数')
        if request.action == 'delete' and request.payload.get('confirmed') is not True:
            raise HTTPException(422, '请先确认删除')
        try:
            payloads = [self.q.validate_payload(request.action, item.payload) for item in request.accounts]
        except ValidationError:
            raise HTTPException(422, '批量操作参数无效') from None
        signature = digest({**request.model_dump(exclude={'request_id'}),
            'accounts': sorted([i.model_dump() for i in request.accounts], key=lambda i: i['account_id'])})
        async with self.q.submit_lock:
            data = self.q.store.read()
            old = data['batch_requests'].get(request.request_id)
            if old:
                if data['batches'][old]['request_hash'] != signature:
                    raise HTTPException(409, '请求 ID 已用于其他批量操作')
                for jid in data['batches'][old]['job_ids']:
                    self.q.keys[jid] = key
                return self.q.listing(old)
            if request.request_id in data['requests']:
                raise HTTPException(409, '请求 ID 已用于其他操作')
            rows = await self.s.actions.accounts(ids)
            children = await self.children(ids) if request.action == 'delete' else {}
            managed = await asyncio.to_thread(self.q.managed) if request.action == 'delete' else set()
            bid, created, jobs = uuid.uuid4().hex, stamp(), []
            for item, payload in zip(request.accounts, payloads):
                row = rows.get(item.account_id)
                job = {'id': uuid.uuid4().hex, 'batch_id': bid, 'account_id': item.account_id,
                    'account_name': str((row or {}).get('name') or f'#{item.account_id}'),
                    'action': request.action, 'payload': payload, 'expected_version': item.expected_version,
                    'client_id': request.client_id, 'request_id': uuid.uuid4().hex,
                    'request_hash': digest([request.action, payload, item.expected_version]),
                    'parent_account_id': (row or {}).get('parent_account_id'),
                    'created_at': created, 'updated_at': created, 'status': 'queued', 'reason': '等待执行', 'result': None}
                if request.action == 'delete':
                    job['child_account_ids'] = children[item.account_id]
                    job['managed_child_ids'] = sorted(set(children[item.account_id]) & managed)
                if self.reached(row, job):
                    job.update(status='completed', reason='', result=self.confirmed(row, job, unchanged=True))
                elif not row or self.q.fingerprint(row, job) != item.expected_version:
                    job.update(status='needs_confirmation', reason='相关字段或账号资格已变化，请重新预览')
                jobs.append(job)
            with self.q.store.transaction() as data:
                data['batches'][bid] = {'id': bid, 'request_hash': signature, 'client_id': request.client_id,
                    'request_id': request.request_id, 'created_at': created, 'job_ids': [j['id'] for j in jobs]}
                data['batch_requests'][request.request_id] = bid
                for job in jobs:
                    if job['action'] == 'groups' and job['status'] == 'queued':
                        for old in data['jobs'].values():
                            if (old['account_id'] == job['account_id'] and old['client_id'] == job['client_id']
                                    and old['action'] == 'groups' and old['status'] in {'queued', 'needs_confirmation'}):
                                old.update(status='superseded', reason='已由本客户端的新请求替代', updated_at=created)
                    data['jobs'][job['id']] = job
                    data['requests'][job['request_id']] = job['id']
            for job in jobs:
                self.q.keys[job['id']] = key
                self.q.audit(job, 'accepted')
            return self.q.listing(bid)

    @staticmethod
    def reached(row, job):
        action = job['action']
        if action == 'delete':
            return row is None
        if not row:
            return False
        if action == 'account_template':
            from .account_templates import eligible
            return bool(eligible(row) and not row.get('passthrough') and
                        (row.get('model_mapping') or {}) == job.get('template_target'))
        if action == 'proxy':
            return (row.get('proxy_id') or None) == job['payload']['proxy_id']
        if 'native_target' in job:
            return sorted(row.get('group_ids') or []) == job['native_target']['group_ids']
        scope = set(job['payload']['scope_group_ids'])
        return set(row.get('group_ids') or []) & scope == set(job['payload']['group_ids'])

    @staticmethod
    def confirmed(row, job, unchanged=False):
        result = {'verified': True, 'account_id': job['account_id']}
        if unchanged:
            result['unchanged'] = True
        if job['action'] == 'delete':
            result.update(deleted=True, detached=bool(job.get('detached')))
        elif job['action'] == 'groups':
            result['group_ids'] = row.get('group_ids') or []
        elif job['action'] == 'proxy':
            result['proxy_id'] = row.get('proxy_id') or None
        else:
            result['applied'] = True
        return result

    async def target(self, row, job, key):
        action, p = job['action'], job['payload']
        if action == 'account_template':
            from .account_templates import eligible, account_version
            if not eligible(row) or row.get('passthrough'):
                raise HTTPException(409, '账号继承或透传状态不允许应用模板')
            target = self.s.account_templates.desired(p)
            if account_version(row) != job['expected_version']:
                raise HTTPException(409, '账号模型配置或凭据已变化')
            return {'credentials': {'model_mapping': target}}
        if self.q.fingerprint(row, job) != job['expected_version']:
            raise HTTPException(409, '相关字段或账号资格已变化')
        if action == 'proxy':
            from .account_settings import capabilities
            if not capabilities(row)['proxy']['editable']:
                raise HTTPException(409, '代理继承母账号，不能独立修改')
            return {'proxy_id': p['proxy_id'] or 0}
        if action == 'delete':
            if row['id'] in self.q.managed() and not p.get('detach_managed'):
                raise HTTPException(409, '请确认解除托管后删除')
            return {}
        scope, selected = set(p['scope_group_ids']), set(p['group_ids'])
        if row['platform'] not in {'openai', 'grok'} or any(i < 1 for i in scope | selected) or not selected <= scope:
            raise HTTPException(409, '分组范围或平台不匹配')
        groups = await asyncio.to_thread(self.s.r.db.fetch_all,
            'SELECT id,platform FROM groups WHERE deleted_at IS NULL AND id=ANY(%(ids)s)', {'ids': sorted(scope)})
        if {g['id'] for g in groups} != scope or any(g['platform'] != row['platform'] for g in groups):
            raise HTTPException(409, '分组已变化，请重新预览')
        return {'group_ids': sorted((set(row.get('group_ids') or []) - scope) | selected)}

    async def run(self, bid):
        lock = self.locks.setdefault(bid, asyncio.Lock())
        if lock.locked():
            return
        async with lock:
            try:
                await self._run(bid)
            except asyncio.CancelledError:
                raise  # Running jobs are reconciled on the next startup.
            except HTTPException as exc:
                from .desktop_api import safe_error_text
                for jid in self.q.store.read()['batches'][bid]['job_ids']:
                    status = self.q.store.read()['jobs'][jid]['status']
                    if status in {'queued', 'running'}:
                        self.q.update(jid, status='checking' if status == 'running' else 'needs_confirmation',
                                      reason=safe_error_text(exc.detail) or '批量操作需要重新确认')
            except Exception:
                # Never turn an unknown dispatched write back into a queued write.
                data = self.q.store.read()
                for jid in data['batches'][bid]['job_ids']:
                    if data['jobs'][jid]['status'] == 'running':
                        self.q.update(jid, status='checking', reason='批量结果待核对，未重复执行')
            finally:
                self.refresh_chunks(bid)

    def refresh_chunks(self, bid):
        with self.q.store.transaction() as data:
            for chunk in data['batches'][bid].get('chunks', []):
                if chunk.get('status') == 'unsupported':
                    continue
                states = [data['jobs'][jid]['status'] for jid in chunk['job_ids']]
                if all(state not in {'running', 'checking', 'queued'} for state in states):
                    chunk['status'] = 'needs_confirmation' if 'needs_confirmation' in states else 'checked'
                elif 'checking' in states:
                    chunk['status'] = 'checking'

    async def _run(self, bid):
        data = self.q.store.read()
        jobs = [copy.deepcopy(data['jobs'][jid]) for jid in data['batches'][bid]['job_ids']
                if self.handles(data['jobs'][jid])]
        if not jobs:
            return
        rows = await self.rows(jobs)
        candidates = []
        for job in jobs:
            row = rows.get(job['account_id'])
            if self.reached(row, job):
                self.q.update(job['id'], status='completed', reason='', result=self.confirmed(row, job, unchanged=job['status'] == 'queued'))
            elif job['status'] == 'checking':
                self.q.update(job['id'], status='needs_confirmation', reason='已派发请求尚未确认，未重复执行', current=self.q.current(row or {}, job))
            elif row:
                candidates.append(job)
            else:
                self.q.update(job['id'], status='needs_confirmation', reason='账号已删除或不存在')
        if not candidates:
            return
        key = self.q.keys.get(candidates[0]['id']) or self.s.r.oauth_state_store().admin_token()
        if not key:
            for job in candidates:
                self.q.update(job['id'], status='needs_confirmation', reason='管理员授权不可用')
            return
        await asyncio.to_thread(self.s.authenticate, key, fresh=True)
        # Each pass is bounded. Busy accounts remain queued while others proceed.
        for start in range(0, len(candidates), 100):
            await self.chunk(bid, candidates[start:start + 100], key)

    async def chunk(self, bid, jobs, key):
        held, admitted = {}, []
        rows = await self.rows(jobs)
        descendants = await self.children([j['account_id'] for j in jobs]) if jobs[0]['action'] == 'delete' else {}
        # Acquire the complete union in a stable order, without waiting. Shared
        # parent leases are held once for the whole native request.
        needed = {}
        for job in jobs:
            row = rows.get(job['account_id'])
            if not row:
                continue
            ids = {row['id'], row.get('parent_account_id') or (row.get('extra') or {}).get('parent_account_id')}
            ids.update(descendants.get(row['id'], []))
            needed[job['id']] = ids - {None, 0}
        try:
            for aid in sorted(set().union(*needed.values()) if needed else set()):
                lease = account_lock(self.s.r.db, aid)
                if lease.acquire(blocking=False):
                    held[aid] = lease
            rows = await self.rows(jobs)
            if descendants:
                descendants = await self.children([j['account_id'] for j in jobs])
            choices = await self.s.account_settings.proxies(key) if jobs[0]['action'] == 'proxy' and jobs[0]['payload']['proxy_id'] is not None else []
            groups = defaultdict(list)
            for job in jobs:
                if self.q.store.read()['jobs'][job['id']]['status'] != 'queued':
                    continue
                row = rows.get(job['account_id'])
                if not needed.get(job['id'], {0}) <= held.keys():
                    self.q.update(job['id'], reason='等待账号空闲')
                    continue
                if not row:
                    continue
                try:
                    parent = row.get('parent_account_id') or (row.get('extra') or {}).get('parent_account_id')
                    if parent and parent not in needed[job['id']]:
                        raise HTTPException(409, '账号继承关系已变化')
                    if job['action'] == 'delete' and descendants[job['account_id']] != job.get('child_account_ids', []):
                        raise HTTPException(409, '关联账号已变化，请重新确认删除影响')
                    if job['action'] == 'proxy' and job['payload']['proxy_id'] is not None and not any(
                            p['id'] == job['payload']['proxy_id'] and p['status'] == 'active' for p in choices):
                        raise HTTPException(409, '所选代理已不可用')
                    target = await self.target(row, job, key)
                    if self.reached(row, job):
                        self.q.update(job['id'], status='completed', reason='', result=self.confirmed(row, job, unchanged=True))
                        continue
                    job['native_target'] = target
                    groups[digest(target)].append(job)
                    admitted.append(job)
                except HTTPException as exc:
                    self.q.update(job['id'], status='needs_confirmation', reason=str(exc.detail))
            for group in groups.values():
                if group[0]['action'] == 'delete':
                    await self.q.thread_write(self.delete_chunk, bid, group, key)
                else:
                    await self.dispatch(bid, group, key)
        finally:
            for lease in reversed(list(held.values())):
                lease.release()
            if admitted:
                await asyncio.to_thread(self.s.invalidate)

    def delete_chunk(self, bid, jobs, key):
        """Keep controller/config guards on one OS thread through deletion."""
        controller = self.s.r.key_fallback_controller
        if controller is None:
            for job in jobs:
                self.q.update(job['id'], status='needs_confirmation', reason='调度控制器未就绪')
            return
        with ExitStack() as stack:
            stack.enter_context(self.s.config.thread_lock)
            stack.enter_context(controller._lock)
            config = controller.load_config()
            if not config.valid:
                raise ValueError('托管配置不可读')
            # Recheck the managed subset under its writer lock.
            rows = asyncio.run(self.s.actions.accounts([j['account_id'] for j in jobs]))
            children = asyncio.run(self.children([j['account_id'] for j in jobs]))
            allowed = []
            for job in jobs:
                row = rows.get(job['account_id'])
                if self.reached(row, job):
                    self.q.update(job['id'], status='completed', reason='', result=self.confirmed(row, job))
                elif (self.q.fingerprint(row, job) != job['expected_version']
                      or children[job['account_id']] != job.get('child_account_ids', [])
                      or sorted(set(children[job['account_id']]) & set(config.managed_account_ids))
                         != job.get('managed_child_ids', [])):
                    self.q.update(job['id'], status='needs_confirmation', reason='账号或关联账号已变化，请重新确认删除')
                elif job['account_id'] in config.managed_account_ids and not job['payload'].get('detach_managed'):
                    self.q.update(job['id'], status='needs_confirmation', reason='托管状态已变化，请重新确认删除')
                else:
                    allowed.append(job)
            if not allowed:
                return
            compatibility = self.q.store.read()['batches'][bid].get('native_unsupported') is True
            if compatibility and len(allowed) > 1:
                for job in allowed:
                    self.delete_chunk(bid, [job], key)
                return
            allowed = self.record(bid, allowed, 'compatibility' if compatibility else 'native')
            if not allowed:
                return
            affected = {j['account_id'] for j in allowed}
            for job in allowed:
                affected.update(job.get('child_account_ids', []))
            detached = affected & set(config.managed_account_ids)
            if detached:
                controller._write_config_unlocked(openai_enabled=config.openai_enabled, grok_enabled=config.grok_enabled,
                    managed_account_ids=[i for i in config.managed_account_ids if i not in detached],
                    config_version=config.config_version + 1, updated_by='desktop:admin')
            monitor = getattr(self.s.r, 'oauth_monitor', None)
            if monitor and hasattr(monitor, 'auto_reset'):
                for aid in sorted(affected):
                    monitor.auto_reset.cancel(aid)
            for job in allowed:
                job['detached'] = job['account_id'] in detached
                if job['detached']:
                    self.q.update(job['id'], detached=True)
            asyncio.run(self.send(bid, allowed, key, compatibility=compatibility, prepared=True))

    def record(self, bid, jobs, mode):
        from .account_operations import stamp
        cid, now, selected = uuid.uuid4().hex, stamp(), []
        with self.q.store.transaction() as data:
            for job in jobs:
                live = data['jobs'][job['id']]
                if live['status'] != 'queued':
                    continue
                live.update(status='running', started_at=now, updated_at=now, reason='',
                            dispatch_id=cid, execution_mode=mode, native_target=job['native_target'])
                if job.get('detached'):
                    live['detached'] = True
                selected.append(copy.deepcopy(live))
            if selected:
                data['batches'][bid].setdefault('chunks', []).append({'id': cid, 'job_ids': [j['id'] for j in selected],
                    'account_ids': [j['account_id'] for j in selected], 'status': 'dispatched', 'execution_mode': mode, 'dispatched_at': now})
        return selected

    async def dispatch(self, bid, jobs, key):
        compatible = self.q.store.read()['batches'][bid].get('native_unsupported') is True
        if compatible:
            for job in jobs:
                await self.send(bid, [job], key, compatibility=True)
            return
        await self.send(bid, jobs, key)

    async def send(self, bid, jobs, key, compatibility=False, prepared=False):
        selected = jobs if prepared else self.record(bid, jobs, 'compatibility' if compatibility else 'native')
        if not selected:
            return
        action = selected[0]['action']
        ids = {j['account_id'] for j in selected}
        path = 'accounts/batch-delete' if action == 'delete' else 'accounts/bulk-update'
        body = {'account_ids': sorted(ids), **selected[0]['native_target']}
        method = 'POST'
        if compatibility:
            path, body = f"accounts/{selected[0]['account_id']}", selected[0]['native_target']
            method = 'DELETE' if action == 'delete' else 'PUT'
        parsed, unavailable, errors = None, False, {}
        try:
            async with self.s.actions.client(key, 30) as client:
                response = await client.request(method, path, json=body)
            if not compatibility and unsupported(response):
                unavailable = True
            elif 200 <= response.status_code < 300 and len(response.content) <= 8_000_000:
                value = response.json()
                if isinstance(value, dict) and type(value.get('code')) is int and value['code'] == 0:
                    parsed = (ids, set()) if compatibility else result_ids(value.get('data'), ids, action)
                    if parsed and not compatibility:
                        from .desktop_api import safe_error_text
                        errors = {item['account_id']: safe_error_text(item.get('error'), 200)
                                  for item in value['data'].get('errors' if action == 'delete' else 'results', [])
                                  if item['account_id'] in parsed[1]}
        except (httpx.HTTPError, ValueError):
            pass
        if unavailable:
            with self.q.store.transaction() as data:
                data['batches'][bid]['native_unsupported'] = True
                for job in selected:
                    data['jobs'][job['id']].update(status='queued', reason='原生批量接口不支持，兼容执行')
                data['batches'][bid]['chunks'][-1]['status'] = 'unsupported'
            for job in selected:
                await self.send(bid, [job], key, compatibility=True)
            return
        # Save the uncertain state before readback, so read failures/restarts can
        # never replay a possibly successful write.
        for job in selected:
            self.q.update(job['id'], status='checking', reason='正在核对批量结果')
        rows = await self.rows(selected)
        for job in selected:
            row = rows.get(job['account_id'])
            if self.reached(row, job):
                status, reason, result = 'completed', '兼容执行' if compatibility else '', self.confirmed(row, job)
            elif parsed and job['account_id'] in parsed[1]:
                status, reason, result = 'failed', errors.get(job['account_id']) or 'Sub2API 未完成此账号操作，请重新预览后重试', None
            else:
                status, reason, result = 'needs_confirmation', '批量写入结果未确认，未重复执行', None
            self.q.update(job['id'], status=status, reason=reason, result=result)
            self.q.audit(job, status)
        with self.q.store.transaction() as data:
            chunk = next(c for c in data['batches'][bid]['chunks'] if c['id'] == selected[0]['dispatch_id'])
            chunk['status'] = 'checked'
            chunk['response_valid'] = parsed is not None
