"""Read-only recovery clock; each account progresses without a batch barrier."""
from __future__ import annotations

import threading
from concurrent.futures import ThreadPoolExecutor, wait
from datetime import timedelta

from .account_ops import recovery_account_states
from .audit import write_audit
from .oauth_queries import automatic_gate, reset_not_before
from .quota_snapshot import latest_openai_result
from .recovery_policy import recovery_method
from .usage_query import parse_iso_datetime


class RecoveryDispatcher:
    def __init__(self, monitor, *, state_loader=recovery_account_states):
        self.m, self.state_loader = monitor, state_loader
        self.lock = threading.Lock()
        self.jobs = {}
        self.credit = None
        self.daily = None
        self.executor = ThreadPoolExecutor(max_workers=24, thread_name_prefix='oauth-recovery')
        self.closed = False

    def tick(self, now=None):
        from .oauth_monitor import build_monitor_candidates, recovery_quota_ready
        current = now or self.m.clock()
        if self.closed or not self.lock.acquire(blocking=False):
            return
        try:
            for aid, job in list(self.jobs.items()):
                if job.done():
                    self._result(job, aid)
                    del self.jobs[aid]
                    def stop_unknown(data):
                        intent = data['scheduler'].get(str(aid), {}).get('recovery_intent') or {}
                        if (intent.get('verification_dispatched') and not intent.get('verification_receipt')
                                and intent.get('status') != 'cancelled'):
                            intent.update(status='blocked', waiting_reason='verification_uncertain',
                                last_error_code='verification_uncertain', next_retry_at='', waiting_until=None)
                    self.m.store.transaction(stop_unknown)
            if self.credit is not None and self.credit.done():
                self._result(self.credit, None)
                self.credit = None
            if self.daily is not None and self.daily.done():
                self._result(self.daily, None)
                self.daily = None
            if not self.m._run_lock.acquire(blocking=False):
                return
            try:
                self.m._refresh_inventory(current)
                # Re-read disk before any dispatch. Missing/corrupt state is a
                # stop condition, never an empty budget or a fresh recovery.
                self.m.store.reload()
                if self.daily is None and self.m.daily_schedule.enabled:
                    batch = self.m.daily_schedule.claim(current)
                    if batch:
                        self.daily = self.executor.submit(self._daily, batch, list(self.m._accounts), current)
                inventory = {int(r['id']): r for r in self.m._accounts if recovery_method(self.m.settings, int(r['id']))}
                live = self.state_loader(self.m.db, list(inventory))
                rows = []
                for value in live:
                    aid = int(value['id'])
                    if aid not in inventory:
                        continue
                    base = inventory[aid]
                    patch = dict(value)
                    extra = patch.pop('recovery_extra', None)
                    if extra is not None:
                        patch['extra'] = {**(base.get('extra') or {}), **extra}
                    rows.append({**base, **patch})
                idle = [row for row in rows if row['id'] not in self.jobs]
                if self.m.settings.oauth_recovery_monitor_enabled:
                    self.m._discover_recoveries(idle, current)
                state = self.m.store.cached_snapshot()
                results = {int(k): v for k, v in state['oauth_results'].items()}
                scheduler = {int(k): v for k, v in state['scheduler'].items()}
                candidates = build_monitor_candidates(idle, results, scheduler, current,
                    recovery_monitor_enabled=self.m.settings.oauth_recovery_monitor_enabled)
                # Do not let ordinary recovery operate on a credit controller's
                # unfinished consumption, including a lost-response restart.
                def credit_held(aid):
                    task = scheduler.get(aid, {}).get('auto_reset_credit') or {}
                    return bool((task.get('owns_pause') or task.get('attempt_at')) and
                                task.get('stage') not in {'recovered', 'closed'})
                candidates = [c for c in candidates if not credit_held(c['account_id'])]
                limit = min(24, max(1, int(getattr(self.m.settings, 'oauth_usage_refresh_concurrency', 4))) +
                            max(1, int(getattr(self.m.settings, 'oauth_recovery_test_concurrency', 2))))
                waiting = {}
                candidate_ids = {c['account_id'] for c in candidates}
                for row in idle:
                    aid = row['id']
                    meta = scheduler.get(aid, {})
                    intent = meta.get('recovery_intent') or {}
                    if not intent or intent.get('status') in {'cancelled', 'recovered', 'blocked', 'auth_failed'}:
                        continue
                    saved = latest_openai_result(row, results.get(aid), current)
                    reason, until = automatic_gate(row, saved, meta, current, wait_for_reset=False)
                    boundary = reset_not_before(row, saved)
                    retry = parse_iso_datetime(intent.get('next_retry_at'))
                    if reason in {'query_cooldown', 'query_budget', 'query_backoff'} and recovery_quota_ready(row, saved, meta, current):
                        reason, until = '', None
                    retry_reason = intent.get('waiting_reason') if intent.get('waiting_reason') in {
                        'model_cooldown', 'model_busy', 'test_slots', 'model_verification_pending'} else 'verification_backoff'
                    bounds = [(t, r) for t, r in ((until, reason), (boundary, 'waiting_reset'), (retry, retry_reason)) if t and t > current]
                    if bounds:
                        until, reason = max(bounds)
                    elif intent.get('waiting_reason') in {'account_busy', 'query_slots', 'test_slots', 'incomplete_quota'}:
                        reason, until = intent['waiting_reason'], None
                    elif aid in candidate_ids and len(self.jobs) >= limit:
                        reason, until = 'worker_slots', current + timedelta(seconds=2)
                    elif not reason:
                        reason, until = '', None
                    updated = {**intent, 'waiting_reason': reason, 'waiting_until': until.isoformat() if until else None}
                    if updated != intent:
                        waiting[aid] = {'recovery_intent': updated}
                if waiting:
                    self.m.store.commit(scheduler_updates=waiting,
                        expected_generations={aid: int((meta.get('manual_control') or {}).get('generation', 0)) for aid, meta in scheduler.items()},
                        expected_recovery_methods={aid: int(meta.get('recovery_method_generation', 0)) for aid, meta in scheduler.items()})
                for candidate in candidates:
                    if len(self.jobs) >= limit:
                        break
                    aid = candidate['account_id']
                    self.jobs[aid] = self.executor.submit(self._account, aid, now)
                if self.credit is None:
                    self.credit = self.executor.submit(self.m.auto_reset.run, rows, current)
            finally:
                self.m._run_lock.release()
        finally:
            self.lock.release()

    def _account(self, aid, fixed_now):
        row = self.m._read_account(aid)
        if not row or self.closed:
            return
        started = self.m.clock() if fixed_now is None else fixed_now
        self.m._run_cycle(started, force=False, live_clock=fixed_now is None, account_rows=[row])

    def _result(self, future, aid):
        try:
            future.result()
        except Exception:
            write_audit(self.m.settings.audit_path, 'oauth_recovery_worker_failed',
                        {'account_id': aid, 'error_code': 'worker_failed'})

    def _daily(self, batch, rows, now):
        from .daily_test import daily_account_eligible
        try:
            generations = {int(r['id']): self.m.store.control_generation(int(r['id'])) for r in rows}
            quota = {}
            for row in rows:
                if not daily_account_eligible(row, now):
                    continue
                self.m._run_cycle(now, force=False, recovery_enabled=False, daily=True,
                                  account_rows=[row], live_clock=True)
                current = self.m.clock()
                live = self.m._read_account(row['id'])
                if live:
                    quota[row['id']] = latest_openai_result(live, self.m.store.results().get(row['id']), current)
            self.m._run_daily_batch(now, batch, {}, rows=rows, usage_results=quota, test_results={}, generations=generations)
        except Exception:
            batch.update(status='failed', error='每日测活执行失败', completed_at=self.m.clock().isoformat())
            self.m.daily_schedule.save_batch(batch)
            raise

    def active(self):
        return (any(not f.done() for f in tuple(self.jobs.values())) or
                any(f is not None and not f.done() for f in (self.credit, self.daily)))

    def drain(self, timeout=40):
        """Bounded join for shutdown/tests; does not submit or replay a task."""
        with self.lock:
            jobs = list(self.jobs.values()) + [f for f in (self.credit, self.daily) if f is not None]
        done, pending = wait(jobs, timeout=timeout)
        return not pending

    def close(self):
        self.closed = True
        self.executor.shutdown(wait=True, cancel_futures=True)


def project_recovery_state(metadata):
    intent = metadata.get('recovery_intent') or {}
    if not intent or intent.get('status') in {'cancelled', 'recovered'}:
        return None
    reason = intent.get('waiting_reason') or intent.get('last_error_code') or ''
    labels = {'waiting_reset': '等待额度恢复时间', 'query_cooldown': '额度查询冷却中',
              'query_budget': '等待额度查询预算', 'query_backoff': '额度查询退避中',
              'query_slots': '等待额度查询执行槽', 'test_slots': '等待验证执行槽',
              'worker_slots': '等待恢复执行槽', 'account_busy': '等待账号操作完成',
              'incomplete_quota': '缺少跨恢复点的完整额度证据', 'verification_backoff': '等待验证重试',
              'model_verification_pending': '等待模型验证', 'model_cooldown': '模型验证冷却中',
              'model_busy': '等待当前模型测试结束',
              'auth_paused': '等待凭据更新', 'http_401': '等待凭据更新', 'http_402': '等待凭据更新',
              'verification_uncertain': '上次验证结果未保存，需人工复核'}
    label = ('验证中' if intent.get('verification_dispatched') and reason != 'verification_uncertain'
             else labels.get(reason, '等待恢复验证'))
    return {'status': str(intent.get('status') or 'pending'), 'label': label,
            'reason': str(reason), 'next_at': intent.get('waiting_until') or intent.get('next_retry_at') or None}
