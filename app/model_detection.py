"""Durable, per-account automatic ModelTrace detection and verified scheduling holds."""
from __future__ import annotations
import asyncio
import uuid
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path
from fastapi import HTTPException
from pydantic import BaseModel, ConfigDict, Field
from .account_locks import control_lock
from .audit import write_audit
from .capacity_alerts import mark_view
from .operation_versions import digest, versions
from .policy_store import PolicyStore
from .usage_query import parse_iso_datetime
from .detection_dispatch import DetectionDispatch
from .recovery_models import RecoveryModels

TARGET = 'gpt-5.6-luna'
DEFAULT_MODEL = 'gpt-6-luna'
ACTIVE = {'queued', 'running', 'retrying'}

def utcnow():
    return datetime.now(timezone.utc)

class DetectionRequest(BaseModel):
    model_config = ConfigDict(extra='forbid')
    expected_version: str = Field(min_length=64, max_length=64)
    enabled: bool = Field(strict=True)
    interval_minutes: int = Field(default=15, ge=1, le=525600, strict=True)
    model_id: str = Field(default=DEFAULT_MODEL, min_length=1, max_length=200, pattern=r'^[A-Za-z0-9._:/-]+$')

class ModelDetection(DetectionDispatch, RecoveryModels):
    def __init__(self, service, *, clock=utcnow):
        self.s, self.r, self.clock = service, service.r, clock
        self.store = PolicyStore(Path(self.r.settings.usage_query_state_path).with_name('model-detection-state.json'),
                                 {'accounts': {}, 'consumed': {}})
        self.tick_lock = asyncio.Lock()
        self._notification_lock = threading.Lock()

    @staticmethod
    def default():
        return {'enabled': False, 'interval_minutes': 15, 'model_id': DEFAULT_MODEL, 'generation': 0,
                'next_at': None, 'status': 'disabled', 'reason': '', 'job_id': None, 'hold': False}

    def data(self):
        data = self.store.read()
        if not isinstance(data.get('accounts'), dict) or not isinstance(data.get('consumed'), dict):
            raise ValueError('模型检测状态无效')
        for value in data['accounts'].values():
            DetectionRequest(expected_version='0'*64, **{k: value[k] for k in ('enabled','interval_minutes','model_id')})
            if type(value.get('generation')) is not int or type(value.get('hold')) is not bool:
                raise ValueError('模型检测控制状态无效')
        return data

    def control(self, aid):
        return self.data()['accounts'].get(str(aid), self.default())

    @staticmethod
    def version(value):
        return digest({k: value[k] for k in ('enabled','interval_minutes','model_id','generation')})

    def view(self, aid):
        value = self.control(aid)
        start = self.data().get('automatic_starts', {}).get(str(aid), {})
        notice = self.data().get('disposition_notifications', {}).get((value.get('disposition') or {}).get('job_id'))
        return {**value, 'version': self.version(value), 'account_id': aid,
                'last_automatic_started_at': start.get('started_at'), 'next_allowed_at': self.next_allowed_at(aid),
                'disposition_notification': notice}

    def held(self, aid):
        try:
            return self.control(aid).get('hold') is True
        except (OSError, ValueError):
            return True  # Unknown ownership cannot authorize automatic scheduling.

    def update(self, aid, **changes):
        with self.store.transaction() as data:
            value = data['accounts'].setdefault(str(aid), self.default())
            value.update(changes)
        self.s.invalidate()
        return value

    def human_control(self, aid, *, release_hold=False):
        with self.store.transaction() as data:
            value = data['accounts'].setdefault(str(aid), self.default())
            value.update(generation=value['generation']+1, pending_request_id=None, status='paused', reason='人工操作后重新核对', next_at=None)
            if release_hold:
                value['hold'] = False
                if value.get('disposition'):
                    value['disposition'].update(status='overridden', reason='人工操作已优先处理')
        self.s.invalidate()

    async def save(self, aid, payload):
        row = await self.s.model_tests.account(aid)
        if payload.enabled and not await self.s.actions.model_allowed(row, payload.model_id, candidate_required=True):
            raise HTTPException(422, '所选模型不在当前分组白名单中')
        def commit():
            with control_lock(self.r.db, aid), self.store.transaction() as data:
                value = data['accounts'].setdefault(str(aid), self.default())
                if self.version(value) != payload.expected_version:
                    raise HTTPException(409, '检测设置已变化，请重新读取')
                value.update(enabled=payload.enabled, interval_minutes=payload.interval_minutes, model_id=payload.model_id,
                             generation=value['generation']+1, pending_request_id=None, candidate_blocked=False, candidate_recheck_at=None, status='waiting' if payload.enabled else 'disabled', reason='',
                             next_at=(self.clock()+timedelta(minutes=payload.interval_minutes)).isoformat() if payload.enabled else None)
        await asyncio.to_thread(commit)
        self.s.invalidate()
        return self.view(aid)

    def mark(self, aid):
        state = self.r.capacity_alerts.store.snapshot()
        return mark_view(aid, state['marks'].get(str(aid)))

    def reason(self, row, mark):
        if not row or row.get('platform') != 'openai' or row.get('type') not in {'oauth','apikey'} or row.get('deleted_at'):
            return '账号已删除或不支持检测'
        if mark.get('marked'):
            return '已标记降智'
        if row.get('status') != 'active':
            return '账号停用或认证异常'
        if row.get('schedulable') is not True:
            return '调度已关闭'
        from .desktop_usage import project_usage
        for window in project_usage(row, self.clock()).get('windows', []):
            used = window.get('used_percent')
            if isinstance(used, (int,float)) and used >= 100:
                return '已确认额度耗尽'
        monitor = getattr(self.r, 'oauth_monitor', None)
        if monitor and row['type'] == 'oauth':
            state = monitor.store.snapshot()
            meta = state.get('scheduler', {}).get(str(row['id']), {})
            from .oauth_queries import credential_fingerprint
            auth = (meta.get('quota_query') or {}).get('auth_fingerprint')
            auth_row = (row if 'credentials' in row else monitor._read_account(int(row['id']))) if auth else None
            if auth and (not auth_row or auth == credential_fingerprint(auth_row)):
                return '认证异常，等待凭据更新'
        return ''

    async def guard(self, job):
        aid = job['account_id']
        row = await self.s.actions.account(aid)
        value, mark = self.control(aid), self.mark(aid)
        if value['generation'] != job.get('detection_generation') or mark['version'] != job.get('detection_mark_version'):
            raise HTTPException(409, '检测已被新的人工操作替代')
        context = job.get('recovery_context')
        reason = (self.recovery_reason(row, context, before_reset=bool(context.get('consume')) and not job.get('attempts'))
                  if context else self.reason(row, mark))
        if reason:
            raise HTTPException(409, reason)
        return row

    def clue_status(self, causes, status, *, reason='', job_id=None):
        clues = [cause for cause in causes if cause.startswith(('error:', 'slow:'))]
        if not clues:
            return
        with self.r.capacity_alerts.store.transaction() as data:
            for key in clues:
                record = {'status': status, 'reason': reason, 'job_id': job_id, 'updated_at': self.clock().isoformat()}
                data.setdefault('detection_clues', {})[key] = record
                if key.startswith('error:'):
                    # No fabricated error or delivery receipt; enrich the actual
                    # collected record with its detection outcome only.
                    data.setdefault('notifications', {})[key.split(':', 1)[1]] = record
            data['detection_clues'] = dict(list(data['detection_clues'].items())[-10000:])
        write_audit(self.r.settings.audit_path, 'model_detection_clue', {'triggers': clues, 'result': status, 'reason': reason, 'job_id': job_id})

    async def trigger(self, aid, causes):
        value = self.control(aid)
        if value.get('job_id'):
            try:
                job = self.s.model_tests.get(value['job_id'])
            except HTTPException as exc:
                if exc.status_code != 404:
                    raise
                job = None
            if job and job['status'] in ACTIVE:
                self.clue_status(causes, 'ignored', reason='已有检测排队或运行')
                return job
        latest = self.s.model_tests.latest(aid)
        if latest and latest['status'] in ACTIVE:
            self.clue_status(causes, 'ignored', reason='已有模型测试排队或运行')
            return latest
        allowed = parse_iso_datetime(self.next_allowed_at(aid))
        if allowed and self.clock() < allowed:
            if 'scheduled' in causes:
                self.update(aid, status='waiting', next_at=allowed.isoformat(), reason='等待自动检测冷却')
            write_audit(self.r.settings.audit_path, 'model_detection_cooldown',
                        {'account_id': aid, 'triggers': causes, 'result': 'deferred' if 'scheduled' in causes else 'ignored'})
            self.clue_status(causes, 'ignored', reason='五分钟冷却中')
            return None
        row = await self.s.actions.account(aid)
        mark = self.mark(aid)
        reason = self.reason(row, mark)
        if reason:
            self.update(aid, status='paused', reason=reason, next_at=None)
            self.clue_status(causes, 'ignored', reason=reason)
            return None
        if not await self.s.actions.model_allowed(row, value['model_id'], candidate_required=True):
            self.update(aid, status='paused', reason='所选模型不在当前模型候选中', next_at=None, candidate_blocked=True, candidate_recheck_at=(self.clock()+timedelta(minutes=1)).isoformat())
            self.clue_status(causes, 'failed', reason='所选模型不在当前候选中')
            return None
        bank = self.r.fingerprint_bank.capture()
        if not any(model['id'] == TARGET for model in bank[0]['models']):
            self.update(aid, status='paused', reason='当前指纹库缺少降智判定模型', next_at=None, candidate_blocked=True, candidate_recheck_at=(self.clock()+timedelta(minutes=1)).isoformat())
            self.clue_status(causes, 'failed', reason='当前指纹库缺少判定目标')
            return None
        # Persist the idempotency key before starting a potentially billable job.
        request_id = value.get('pending_request_id') or uuid.uuid4().hex
        self.update(aid, pending_request_id=request_id, candidate_blocked=False, candidate_recheck_at=None, triggers=causes, status='queued', reason='等待检测', next_at=None)
        from .model_tests import ModelTestRequest
        payload = ModelTestRequest(model_id=value['model_id'], expected_version='0'*64,
            expected_operation_version=versions(row)['model_test'], request_id=request_id, concurrency=1)
        job = await self.s.model_tests.start(aid, payload, automatic={
            'detection_generation': value['generation'], 'detection_mark_version': mark['version'], 'triggers': causes})
        self.update(aid, job_id=job['id'], pending_request_id=None, status=job['status'], reason=job.get('error',''))
        self.clue_status(causes, 'testing', job_id=job['id'])
        return job

    async def verdict(self, job, report):
        if not report or report.get('used_outputs', 0) < 1 or report.get('prediction') != TARGET:
            return None
        await self.guard(job)
        aid = job['account_id']
        def prepare():
            from .desktop_api import ACCOUNT_SQL
            with control_lock(self.r.db, aid):
                value, mark = self.control(aid), self.mark(aid)
                if value['generation'] != job['detection_generation'] or mark['version'] != job['detection_mark_version']:
                    raise HTTPException(409, '人工操作已变化，旧检测不再处置')
                row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter='AND a.id=%(id)s'), {'id':aid})
                reason = self.recovery_reason(row, job['recovery_context']) if job.get('recovery_context') else self.reason(row, mark)
                if reason:
                    raise HTTPException(409, reason)
                disposition = {'status':'pending', 'job_id':job['id'], 'generation':value['generation'],
                               'mark_version':mark['version'], 'marked':False, 'schedule_verified':False,
                               'created_at':self.clock().isoformat(), 'model_id': job['requested_model'],
                               'prediction': report['prediction'], 'probability': report.get('probability'),
                               'triggers': job.get('triggers', [])}
                self.update(aid, hold=True, disposition=disposition, status='handling', reason='正在标记并停止调度')
        await asyncio.to_thread(prepare)
        return await asyncio.to_thread(self.finish_disposition, aid)

    def finish_disposition(self, aid):
        from .desktop_api import ACCOUNT_SQL
        from .key_fallback import execute_sub2api_set_schedulable
        with control_lock(self.r.db, aid):
            value = self.control(aid)
            d = value.get('disposition')
            if not d or d['status'] not in {'pending','marked','writing','checking'}:
                return d
            if d['generation'] != value['generation']:
                d.update(status='overridden', reason='人工操作已变化')
                self.update(aid, disposition=d)
                return d
            row = self.r.db.fetch_one(ACCOUNT_SQL.format(filter='AND a.id=%(id)s'), {'id':aid})
            if not row or row['platform'] != 'openai' or row['type'] not in {'oauth','apikey'}:
                d.update(status='needs_confirmation', reason='账号资格已变化')
                self.update(aid, disposition=d)
                return d
            if not d['marked']:
                current = self.mark(aid)
                # A restart between mark persistence and receipt is safe: only our
                # recorded mark origin can be adopted as the same mutation.
                state = self.r.capacity_alerts.store.snapshot()['marks'].get(str(aid), {})
                if current['version'] != d['mark_version'] and state.get('detection_job_id') != d['job_id']:
                    d.update(status='needs_confirmation', reason='降智标记已变化')
                    self.update(aid, disposition=d)
                    return d
                if not current['marked']:
                    self.r.capacity_alerts.store.set_mark(aid, True, current['version'], self.clock(), detection_job_id=d['job_id'])
                d.update(marked=True, status='marked')
                self.update(aid, disposition=d)
            monitor = getattr(self.r, 'oauth_monitor', None)
            if row['type'] == 'oauth' and monitor and not d.get('recovery_revoked'):
                monitor.store.manual_control(row, False, self.clock())
                monitor._inventory_loaded_at = None
                d['recovery_revoked'] = True
                self.update(aid, disposition=d)
            if row['schedulable'] is False:
                d.update(status='completed', schedule_verified=True, reason='')
            elif d['status'] in {'writing','checking'}:
                d.update(status='needs_confirmation', reason='停调度结果未确认，未重复写入')
            else:
                token = self.r.oauth_state_store().admin_token()
                if not token:
                    d.update(status='needs_confirmation', reason='管理员授权不可用')
                else:
                    d['status'] = 'writing'
                    self.update(aid, disposition=d)
                    execute_sub2api_set_schedulable(aid, False, base_url=self.r.oauth_base_url(), admin_token=token, timeout_seconds=3)
                    live = self.r.db.fetch_one(ACCOUNT_SQL.format(filter='AND a.id=%(id)s'), {'id':aid})
                    if live and live['schedulable'] is False:
                        d.update(status='completed', schedule_verified=True, reason='')
                    else:
                        d.update(status='checking', reason='停调度待核对')
            self.commit_disposition(aid, d, row)
            write_audit(self.r.settings.audit_path, 'model_detection_disposition',
                        {'account_id':aid, 'job_id':d['job_id'], 'marked':d['marked'], 'schedule_verified':d['schedule_verified'], 'status':d['status']})
            return d

    async def tick(self):
        async with self.tick_lock:
            alerts = self.r.capacity_alerts.store.snapshot()
            events = alerts.get('detection_events', {})
            for key, event in events.items():
                data = self.data()
                if key not in data['consumed']:
                    try:
                        await self.trigger(event['account_id'], [key])
                    except HTTPException as exc:
                        self.update(event['account_id'], status='paused', reason=str(exc.detail)[:200])
                        self.clue_status([key], 'failed', reason=str(exc.detail)[:200])
                    with self.store.transaction() as data:
                        data['consumed'][key] = self.clock().isoformat()
                        data['consumed'] = dict(list(data['consumed'].items())[-10000:])
                with self.r.capacity_alerts.store.transaction() as data:
                    data.setdefault('detection_events', {}).pop(key, None)
            for key, value in self.data()['accounts'].items():
                aid = int(key)
                if (value.get('disposition') or {}).get('status') in {'pending','marked','writing','checking'}:
                    await asyncio.to_thread(self.finish_disposition, aid)
                    value = self.control(aid)
                job = None
                if value.get('job_id'):
                    try:
                        job = self.s.model_tests.get(value['job_id'])
                    except HTTPException:
                        # A task summary may be pruned independently of the
                        # detection state. Forget only the stale pointer and
                        # allow the account's saved timer to continue.
                        self.update(aid, job_id=None, status='waiting' if value['enabled'] else 'disabled', reason='')
                        value = self.control(aid)
                if job and job['status'] in ACTIVE:
                    if (value.get('disposition') or {}).get('job_id') == job['id']:
                        continue
                    if value['status'] != job['status']:
                        self.update(aid, status=job['status'])
                    try:
                        await self.guard(job)
                    except HTTPException:
                        await self.s.model_tests.cancel(job['id'])
                    continue
                if job and value.get('handled_job') != job['id']:
                    report = job.get('report') or {}
                    valid = job['status'] == 'completed' and report.get('used_outputs', 0) > 0
                    confirmed = job.get('first_group_prediction') == TARGET and valid
                    self.clue_status(job.get('triggers', []), 'confirmed' if confirmed else 'ignored' if valid else 'failed',
                                     reason='首组指纹确认降智' if confirmed else '未确认降智' if valid else job.get('error') or '分析不足', job_id=job['id'])
                    timer = {}
                    if value['generation'] == job.get('detection_generation'):
                        ended = parse_iso_datetime(job.get('completed_at')) or self.clock()
                        timer = {'status':'waiting' if value['enabled'] else 'disabled',
                                 'next_at':(max(ended, self.clock())+timedelta(minutes=value['interval_minutes'])).isoformat() if value['enabled'] else None}
                        allowed = self.next_allowed_at(aid)
                        if timer['next_at'] and allowed:
                            timer['next_at'] = max(parse_iso_datetime(timer['next_at']), parse_iso_datetime(allowed)).isoformat()
                    self.update(aid, handled_job=job['id'], **timer,
                                last_result={'id':job['id'], 'disposition':job.get('automatic_disposition'), 'status':job['status'], 'report':job.get('report'), 'completed_at':job.get('completed_at'),
                                             'error':job.get('error'), 'bank_version':job.get('bank_version')})
                    value = self.control(aid)
                if value.get('hold'):
                    d = value.get('disposition') or {}
                    reason = '已标记降智，停调度已确认' if d.get('schedule_verified') else d.get('reason', '自动处置待核对')
                    if value['status'] != 'paused' or value.get('next_at') or value.get('reason') != reason:
                        self.update(aid, status='paused', next_at=None, reason=reason)
                    continue
                if not value['enabled']:
                    continue
                try:
                    row = await self.s.actions.account(aid)
                    reason = self.reason(row, self.mark(aid))
                    if reason:
                        if value['status'] != 'paused' or value.get('reason') != reason:
                            self.update(aid, status='paused', reason=reason, next_at=None)
                        continue
                    if value.get('candidate_blocked'):
                        recheck = parse_iso_datetime(value.get('candidate_recheck_at'))
                        if recheck and self.clock() < recheck:
                            continue
                        allowed = await self.s.actions.model_allowed(row, value['model_id'], candidate_required=True)
                        bank = self.r.fingerprint_bank.capture()
                        if not allowed or not any(model['id'] == TARGET for model in bank[0]['models']):
                            self.update(aid, candidate_recheck_at=(self.clock()+timedelta(minutes=1)).isoformat())
                            continue
                        self.update(aid, candidate_blocked=False, candidate_recheck_at=None)
                    due = parse_iso_datetime(value.get('next_at'))
                    if due is None:
                        self.update(aid, status='waiting', reason='', next_at=(self.clock()+timedelta(minutes=value['interval_minutes'])).isoformat())
                    elif self.clock() >= due:
                        await self.trigger(aid, ['scheduled'])
                except HTTPException as exc:
                    self.update(aid, status='paused', reason=str(exc.detail)[:200], next_at=None)

    async def loop(self):
        while True:
            try:
                await self.tick()
            except (OSError, ValueError, TypeError):
                write_audit(self.r.settings.audit_path, 'model_detection_state_unavailable', {})
            except Exception as exc:
                write_audit(self.r.settings.audit_path, 'model_detection_failed', {'kind':type(exc).__name__})
            await asyncio.sleep(2)
