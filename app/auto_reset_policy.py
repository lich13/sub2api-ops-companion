"""Finite automatic credit episodes and durable, independently delivered results."""
from __future__ import annotations

from datetime import timedelta

from .oauth_queries import AUTH_ERRORS, credential_fingerprint
from .recovery_policy import recovery_method
from .usage_query import parse_iso_datetime
from .reset_credit_observation import available_credits

WINDOW_SECONDS = 300
TERMINAL = {'finished', 'recovered', 'closed', 'manual', 'blocked'}
LEGACY_ENDED = {'finished', 'recovered', 'closed', 'manual'}
RESULT_LABELS = {
    'restored': '用卡成功，已恢复调度', 'not_executed': '未执行用卡',
    'consume_failed': '用卡明确失败', 'unrecovered': '用卡成功，恢复未完成',
    'unknown': '消费结果不确定，禁止重复用卡',
}
REASONS = {
    'consume_deadline': '已超过五分钟用卡窗口，未补执行',
    'verification_deadline': '已超过五分钟验证期限', 'verification_limit': '两轮验证均未确认恢复',
    'verification_interrupted': '验证曾派发但结果未保存，已停止自动续接',
    'manual_intervention': '人工操作已变更', 'account_ineligible': '账号资格或恢复设置已变化',
    'block_cleared': '本轮限流或耗尽已解除', 'clock_untrusted': '时钟或触发时间无法可靠确认',
    'supplement_limit': '本轮卡证据补查已使用，未重复查询',
    'notification_failed': '通知发送失败', 'account_deleted': '账号已删除或不存在',
    'account_disabled': '账号已停用', 'account_error': '账号状态异常',
    'query_cooldown': '查询冷却超出本轮用卡截止时间', 'query_budget': '剩余查询预算不足',
    'model_cooldown': '模型冷却无法在截止前完成', 'detection_held': '账号已标记降智或被检测停调度',
    'http_401': '上游认证失败', 'http_402': '上游拒绝账号额度',
    'query_busy': '额度读取仍在执行', 'recovery_ineligible': '恢复验证条件已变化',
    'invalid_model_or_request': '模型或请求参数被上游拒绝', 'rate_limited': '验证遭到上游限流',
    'model_analysis_insufficient': '模型验证未获得有效指纹样本',
    'model_request_failed': '模型验证请求失败', 'protocol_mismatch': '模型响应协议不兼容',
    'http_400': '验证请求被上游拒绝', 'http_403': '上游拒绝访问',
    'http_404': '验证接口或模型不存在', 'http_422': '验证请求参数无效',
    'http_429': '验证受到上游限流', 'incomplete_response': '模型响应未完整结束',
    'model_unavailable': '验证模型不在当前分组白名单中', 'bank_unavailable': '指纹库缺少判定目标',
    'account_changed': '账号配置已变化', 'recovery_state_changed': '恢复状态已变化',
    'credit_unconfirmed': '没有可信的可用重置卡观测',
    'invalid_credit_snapshot': '重置卡余额或到期信息无效',
}


def verification_retryable(result, method):
    if type(result.get('retryable')) is bool:
        return result['retryable']
    if method == 'model':
        return False
    code = str(result.get('error_code') or '')
    if code in {'network_error', 'timeout', 'missing_success_event', 'account_test_error'}:
        return True
    status = code.removeprefix('http_')
    return status.isascii() and status.isdigit() and 500 <= int(status) <= 599


class BoundedCreditPolicy:
    def credit_candidate_ready(self, row, now):
        try:
            task = self.state(int(row['id']))
            if task and not (task.get('credit_gate_skipped') and not task.get('owns_pause')):
                return True  # Existing work may need reconciliation, never another card.
            if not getattr(self.m.settings, 'oauth_auto_reset_credit_enabled', False):
                return False
            evidence = self._depletion(row, now)
            if not evidence:
                self._observe_credit_recovery(row, now)
                return False
            return bool(self._admit_credit_episode(row, evidence, now))
        except (OSError, ValueError, TypeError):
            # Admission must be durable before allocating even a worker slot.
            # An unreadable state or failed save never falls back to execution.
            return False

    def _positive_credit_reason(self, row, evidence, now):
        count = available_credits((row.get('extra') or {}).get('codex_reset_credit_snapshot'), now)
        if count is None:
            return 'credit_unconfirmed'
        if count <= 0:
            return 'no_credit'
        return 'credit_unconfirmed' if self._credit_reason(row, evidence, now) else ''

    def _admit_credit_episode(self, row, evidence, now):
        """Remember the depletion clock without creating a task or doing I/O upstream."""
        aid = int(row['id'])
        reason = self._positive_credit_reason(row, evidence, now)
        observed = parse_iso_datetime(evidence.get('evidence_at'))
        if not observed:
            return None
        changed = False
        def remember(data):
            nonlocal changed
            meta = data['scheduler'].setdefault(str(aid), {})
            previous = meta.get('credit_admission') or {}
            recovered = parse_iso_datetime(previous.get('recovered_at'))
            is_new = not previous or recovered and observed > recovered
            trigger = observed if is_new else parse_iso_datetime(previous['trigger_at'])
            checked = max(now, parse_iso_datetime(previous.get('checked_at')) or now)
            why = reason
            if now < (parse_iso_datetime(previous.get('checked_at')) or now) - timedelta(seconds=1):
                why = 'clock_untrusted'
            elif not trigger or checked >= trigger + timedelta(seconds=WINDOW_SECONDS):
                why = 'consume_deadline'
            elif recovered and observed <= recovered:
                why = 'block_cleared'
            value = {'trigger_at': trigger.isoformat(), 'checked_at': checked.isoformat(),
                     'reason': why, 'evidence_fingerprint': evidence['evidence_fingerprint'] if is_new else previous['evidence_fingerprint']}
            if not is_new and recovered:
                value['recovered_at'] = recovered.isoformat()
            changed = {k: v for k, v in previous.items() if k != 'checked_at'} != {k: v for k, v in value.items() if k != 'checked_at'}
            meta['credit_admission'] = value
            return None if why else {**evidence, 'evidence_at': trigger.isoformat()}
        result = self.store.transaction(remember)
        if changed:
            self._audit(aid, 'credit_admitted' if result else 'credit_skipped',
                        reason=self.store.snapshot()['scheduler'][str(aid)]['credit_admission']['reason'])
        return result

    def _observe_credit_recovery(self, row, now):
        from .oauth_monitor import account_recovery_confirmed
        if not row or not account_recovery_confirmed(row) or not self._available(row, now, {}):
            return
        aid = int(row['id'])
        value = self.store.snapshot()['scheduler'].get(str(aid), {}).get('credit_admission')
        if value and not value.get('recovered_at'):
            def save(data):
                current = data['scheduler'][str(aid)].get('credit_admission')
                if current == value:
                    current['recovered_at'] = now.isoformat()
            self.store.transaction(save)

    def _migrate_credit_admission(self, aid, row, task, now):
        # Never gate verification on a balance that the successful consumption
        # just reduced to zero. Manual receipts and unknown writes are immutable.
        if (not row or not task or task.get('attempt_at') or task.get('consumed')
                or task.get('credit_admitted_at') or self._manual_task(task)
                or task.get('stage') in {'recovered', 'closed'}):
            return task
        reason = self._positive_credit_reason(row, task, now)
        if not reason:
            return task
        if task.get('credit_gate_skipped'):
            return task
        updated = {**task, 'revision': int(task.get('revision') or 0) + 1, 'credit_gate_skipped': True}
        if task.get('stage') not in LEGACY_ENDED:
            updated.update(stage='finished', outcome='not_executed', result_reason=reason,
                           error_code=reason, completed_at=now.isoformat(), next_at=None)
        def save(data):
            meta = data['scheduler'].setdefault(str(aid), {})
            current = meta.get('auto_reset_credit') or {}
            if current.get('episode') != task['episode'] or current.get('revision') != task.get('revision'):
                raise ValueError('用卡任务已变化')
            meta['auto_reset_credit'] = updated
            trigger = task.get('trigger_at') or task.get('evidence_at')
            if parse_iso_datetime(trigger):
                meta.setdefault('credit_admission', {'trigger_at': trigger, 'checked_at': now.isoformat(),
                    'reason': reason, 'evidence_fingerprint': task.get('evidence_fingerprint', '')})
            notice = meta.get('credit_result_notifications', {}).get(task['episode'])
            if notice and notice.get('status') == 'pending' and notice.get('outcome') == 'not_executed':
                notice.update(status='suppressed', next_at=None)
        self.store.transaction(save)
        self._audit(aid, 'credit_admission_migrated', reason=reason)
        return updated

    def _manual_task(self, task):
        return bool(task.get('source') == 'manual' or (
            not task.get('policy_version') and task.get('stage') in {'manual', 'uncertain'}
            and not task.get('evidence_at') and not task.get('owns_pause')))

    def _policy(self, aid, task, now):
        if not task or self._manual_task(task) or task.get('policy_version') or task.get('stage') in LEGACY_ENDED:
            return task
        trigger = parse_iso_datetime(task.get('evidence_at'))
        completed = parse_iso_datetime(task.get('reset_completed_at'))
        meta = self.store.snapshot()['scheduler'].get(str(aid), {})
        return self._save(aid, task, policy_version=1,
            trigger_at=trigger.isoformat() if trigger else None,
            consume_deadline_at=(trigger + timedelta(seconds=WINDOW_SECONDS)).isoformat() if trigger else None,
            verification_deadline_at=(completed + timedelta(seconds=WINDOW_SECONDS)).isoformat() if completed else None,
            policy_credential_fingerprint=credential_fingerprint(self.m._read_account(aid) or {}),
            policy_control_generation=(task['control_generation'] if task.get('control_generation') is not None
                                       else self.store.control_generation(aid)),
            policy_detection_version=getattr(self.m, 'credit_control_version', lambda _: None)(aid),
            policy_method=recovery_method(self.m.settings, aid),
            policy_method_generation=int(meta.get('recovery_method_generation', 0)),
            verification_dispatched=bool(task.get('verification_dispatched') or (
                task.get('stage') == 'testing' and int(task.get('test_attempts') or 0) > 0
                and not task.get('test_completed_at'))),
            policy_checked_at=now.isoformat())

    def _policy_now(self, task, now):
        return max(now, parse_iso_datetime(task.get('policy_checked_at')) or now)

    def _expired(self, task, now):
        now = self._policy_now(task, now)
        key = 'verification_deadline_at' if task.get('consumed') else 'consume_deadline_at'
        deadline = parse_iso_datetime(task.get(key))
        # A dispatched consumption can finish after its admission deadline. It
        # gets its own finite verification window only on a confirmed receipt.
        if task.get('attempt_at') and not task.get('consumed'):
            return False
        return not deadline or now >= deadline

    def _result(self, task):
        if task.get('consumed'):
            return 'unrecovered'
        if task.get('attempt_at'):
            return 'consume_failed' if task.get('consumption_outcome') == 'not_consumed' else 'unknown'
        return 'not_executed'

    def _notice(self, aid, task, now, row=None):
        from .auto_reset import ERRORS
        code = task.get('result_reason') or task.get('error_code') or ''
        return {'episode': task['episode'], 'account_id': aid, 'account_name': str((row or {}).get('name') or task.get('account_name') or ''),
                'outcome': task['outcome'], 'reason': REASONS.get(code, ERRORS.get(code, '操作未确认')) if code else '',
                'reason_code': code, 'trigger_at': task.get('trigger_at'), 'attempt_at': task.get('attempt_at'),
                'reset_completed_at': task.get('reset_completed_at'), 'completed_at': now.isoformat(),
                'verification_method': task.get('verification_method') or task.get('policy_method'),
                'model_id': task.get('model_id'), 'status': 'pending', 'attempts': 0,
                'next_at': now.isoformat()}

    def _finish(self, aid, task, reason, now, row=None, *, cancel_verification=True, notice_fields=None):
        if task.get('stage') == 'finished' or (task.get('stage') in LEGACY_ENDED and not task.get('policy_version')):
            return task
        finished = {**task, 'revision': int(task.get('revision') or 0) + 1, 'stage': 'finished',
                    'outcome': self._result(task), 'result_reason': reason, 'error_code': reason,
                    'completed_at': now.isoformat(), 'next_at': None}
        if reason == 'ownership_changed':
            finished['owns_pause'] = False
        notice = {**self._notice(aid, finished, now, row), **(notice_fields or {})}
        def save(data):
            meta = data['scheduler'].setdefault(str(aid), {})
            current = meta.get('auto_reset_credit') or {}
            if current.get('episode') != task['episode'] or current.get('revision') != task.get('revision'):
                raise ValueError('用卡任务已变化')
            meta['auto_reset_credit'] = finished
            meta.setdefault('credit_result_notifications', {}).setdefault(task['episode'], notice)
        self.store.transaction(save)
        self._audit(aid, 'finished', outcome=finished['outcome'], reason=reason)
        cancel = getattr(self.m, 'cancel_credit_verification', None)
        if cancel_verification and cancel and task.get('model_test_job_id'):
            cancel(task['model_test_job_id'], task['episode'])
        return finished

    def _guard_reason(self, aid, row, task, now):
        meta = self.store.snapshot()['scheduler'].get(str(aid), {})
        checked = parse_iso_datetime(task.get('policy_checked_at'))
        if checked and self.m.clock() < checked - timedelta(seconds=1):
            return 'clock_untrusted'
        if not row or row.get('deleted_at'):
            return 'account_deleted'
        if row.get('status') == 'disabled':
            return 'account_disabled'
        if row.get('status') == 'error':
            return 'account_error'
        query = meta.get('quota_query') or {}
        if (query.get('auth_fingerprint') == credential_fingerprint(row)
                or not query.get('auth_fingerprint') and meta.get('last_error_code') in AUTH_ERRORS):
            return 'auth_paused'
        if (self.store.control_generation(aid) != task.get('policy_control_generation')
                or int(meta.get('recovery_method_generation', 0)) != task.get('policy_method_generation')
                or recovery_method(self.m.settings, aid) != task.get('policy_method')):
            return 'manual_intervention'
        if row and task.get('policy_credential_fingerprint') != credential_fingerprint(row):
            return 'ownership_changed'
        if task.get('policy_detection_version') != getattr(self.m, 'credit_control_version', lambda _: None)(aid):
            return 'manual_intervention'
        if getattr(self.m, 'detection_gate', lambda _: False)(aid):
            return 'detection_held'
        if (not getattr(self.m.settings, 'oauth_auto_reset_credit_enabled', False)
                or not self._eligible(row, now, held=bool(task.get('owns_pause') or task.get('stage') == 'pausing'))):
            return 'account_ineligible'
        if task.get('owns_pause') and task.get('stage') != 'releasing' and not self._owned(row, task, same_version=False):
            return 'ownership_changed'
        return ''

    def sweep(self, rows, now):
        """Short state-only watchdog, also runs while network workers are busy."""
        live = {int(r['id']): r for r in rows}
        for key, meta in self.store.snapshot()['scheduler'].items():
            task = meta.get('auto_reset_credit') or {}
            aid = int(key)
            if task and not task.get('attempt_at') and not task.get('credit_admitted_at'):
                row = live.get(aid) or self.m._read_account(aid)
                task = self._migrate_credit_admission(aid, row, task, now)
            if not task or self._manual_task(task) or (task.get('stage') in LEGACY_ENDED and not task.get('policy_version')) or task.get('stage') in {'finished', 'recovered', 'closed'}:
                continue
            aid = int(key)
            if aid not in live:
                # A task can outlive its recovery-group selection. Read its
                # actual row once before reporting it as deleted or ineligible.
                live[aid] = self.m._read_account(aid)
            task = self._policy(aid, task, now)
            current = self._policy_now(task, now)
            control = meta.get('manual_control') or {}
            expected = task.get('policy_control_generation', 0)
            finishing_degradation = bool(task.get('degradation_pending') and (
                int(control.get('generation', 0)) == expected or
                (control.get('origin') == 'model_detection' and int(control.get('generation', 0)) == expected + 1)))
            reason = '' if finishing_degradation else self._guard_reason(aid, live.get(aid), task, current)
            if not reason and self._expired(task, current):
                reason = 'verification_deadline' if task.get('consumed') else 'consume_deadline'
            if task.get('stage') in {'manual', 'blocked'} and not finishing_degradation:
                reason = reason or task.get('error_code') or 'manual_intervention'
            if reason:
                try:
                    self._finish(aid, task, reason, current, live.get(aid))
                except ValueError:
                    pass  # another controller committed; next tick re-reads
            elif task.get('policy_checked_at') != current.isoformat():
                # Do not change request revisions while a worker is in flight.
                def clock(data):
                    value = data['scheduler'].get(key, {}).get('auto_reset_credit') or {}
                    if value.get('episode') == task['episode']:
                        value['policy_checked_at'] = current.isoformat()
                self.store.transaction(clock)

    def _bounded_step(self, aid, now, enabled):
        from .oauth_monitor import account_recovery_confirmed
        row, task = self.m._read_account(aid), self.state(aid)
        task = self._reconcile_parameter_edit(row, task)
        task = self._migrate_credit_admission(aid, row, task, now)
        self._observe_credit_recovery(row, now)
        if task.get('credit_gate_skipped') and not task.get('owns_pause') and enabled:
            evidence = self._depletion(row, now) if row else None
            if evidence:
                task = self._begin_episode(aid, task, evidence)
                if not task:
                    return
        if ((self._manual_task(task) and task.get('stage') not in {'recovered', 'closed'})
                or (task.get('stage') == 'manual' and not task.get('policy_version'))):
            # Preserve old/manual receipts without producing a new automatic
            # notification. A later, independently confirmed recovery may close
            # this episode; an unknown receipt is never promoted to consumed.
            if (row and self._eligible(row, now) and account_recovery_confirmed(row)
                    and self._available(row, now, task)):
                self._save(aid, task, stage='recovered', recovered_at=now.isoformat(), next_at=None)
                return
            control = self.store.snapshot()['scheduler'].get(str(aid), {}).get('manual_control') or {}
            changed = parse_iso_datetime(control.get('at'))
            evidence = self._depletion(row, now) if row and enabled and not task.get('attempt_at') else None
            if (not evidence or control.get('enabled') is not True or not changed
                    or parse_iso_datetime(evidence['evidence_at']) <= changed
                    or evidence['evidence_fingerprint'] == task.get('evidence_fingerprint')):
                return
            task = self._begin_episode(aid, task, evidence)
            if not task:
                return
        if task.get('stage') == 'finished':
            if task.get('owns_pause') and not task.get('attempt_at'):
                self._undo_unissued_pause(aid, row, task, now)
                task = self.state(aid)
                row = self.m._read_account(aid)
            # A fresh observed recovery is the sole way to end a spent episode.
            # A new 429 by itself must never grant another five-minute window.
            if (row and self._eligible(row, now) and account_recovery_confirmed(row)
                    and self._available(row, now, task) and not task.get('recovered_evidence_at')):
                self._save(aid, task, recovered_evidence_at=now.isoformat())
                return
            recovered = parse_iso_datetime(task.get('recovered_evidence_at'))
            evidence = self._depletion(row, now) if row and recovered and enabled else None
            if not evidence or parse_iso_datetime(evidence['evidence_at']) <= recovered:
                return
            task = self._begin_episode(aid, task, evidence)
            if not task:
                return
        if not task or task.get('stage') in {'recovered', 'closed'}:
            evidence = self._depletion(row, now) if row and enabled else None
            if not evidence:
                return
            boundary = parse_iso_datetime(task.get('recovered_at'))
            control = self.store.snapshot()['scheduler'].get(str(aid), {}).get('manual_control') or {}
            changed = parse_iso_datetime(control.get('at'))
            if any(t and parse_iso_datetime(evidence['evidence_at']) <= t for t in (boundary, changed)):
                return
            task = self._begin_episode(aid, task, evidence)
            if not task:
                return
        # Old terminal history has no new notification or business execution.
        if task.get('stage') in LEGACY_ENDED and not task.get('policy_version'):
            return
        task = self._policy(aid, task, now)
        now = self._policy_now(task, now)
        reason = self._guard_reason(aid, row, task, now)
        if reason:
            self._finish(aid, task, reason, now, row); return
        if self._expired(task, now):
            self._finish(aid, task, 'verification_deadline' if task.get('consumed') else 'consume_deadline', now, row); return
        if (row.get('extra') or {}).get('auto_reset_credit_enabled'):
            self._finish(aid, task, 'conflict', now, row); return
        if task.get('stage') == 'releasing':
            self._reconcile_release(aid, task, now); return
        if task.get('attempt_at'):
            if not task.get('consumed'):
                self._finish(aid, task, task.get('error_code') or 'result_uncertain', now, row); return
            due = parse_iso_datetime(task.get('next_at'))
            if due and due > now:
                return
            if task.get('test_success'):
                if self._available(row, now, task):
                    self._release(row, task, now)
                else:
                    self._finish(aid, task, 'incomplete_quota', now, row)
                return
            if task.get('verification_dispatched') and not task.get('model_test_job_id'):
                self._finish(aid, task, 'verification_interrupted', now, row); return
            if int(task.get('test_attempts') or 0) >= 2:
                self._finish(aid, task, 'verification_limit', now, row); return
            self._test(row, task, now); return
        evidence = self._depletion(row, now, held=bool(task.get('owns_pause')))
        if not evidence:
            self._finish(aid, task, 'block_cleared', now, row); return
        if task.get('stage') == 'pausing' or task.get('owns_pause'):
            self._finish(aid, task, 'pause_uncertain', now, row); return
        # Current evidence may change; the original trigger and deadline never do.
        if any(task.get(k) != evidence[k] for k in ('reset_at', 'evidence_fingerprint', 'evidence_source')):
            task = self._save(aid, task, **{k: v for k, v in evidence.items() if k != 'evidence_at'})
        due = parse_iso_datetime(task.get('next_at'))
        if due and due > now:
            return
        credit_reason = self._positive_credit_reason(row, evidence, now)
        if credit_reason:
            self._finish(aid, task, credit_reason, now, row); return
        now = self._policy_now(task, self.m.clock())
        if self._expired(task, now):
            self._finish(aid, task, 'consume_deadline', now, row); return
        if recovery_method(self.m.settings, aid) == 'model':
            verifier = getattr(self.m, 'model_verifier', None)
            if not verifier:
                self._finish(aid, task, 'model_verifier_unavailable', now, row); return
            result = verifier(aid, kind='credit', fingerprint=task['episode'], attempt=1, consume=True)
            task = self.state(aid)
            if task.get('stage') == 'finished':
                return
            if result.get('job_id'):
                def remember(data):
                    current = data['scheduler'][str(aid)]['auto_reset_credit']
                    if current.get('episode') == task['episode']:
                        current['model_test_job_id'] = result['job_id']
                self.store.transaction(remember)
                task = self.state(aid)
            if result.get('deferred'):
                if task.get('stage') not in {'pausing', 'resetting'}:
                    self._bounded_wait(aid, task, result, self.m.clock())
                return
            if task.get('consumed'):
                self._test(row, task, self.m.clock())
            else:
                self._finish(aid, task, result.get('error_code') or 'result_uncertain', self.m.clock(), row)
            return
        # Reserve a connection-test slot through consumption and first verification.
        with self.m._test_slot_lock:
            if self.m._active_tests >= max(1, int(getattr(self.m.settings, 'oauth_recovery_test_concurrency', 2))):
                self._bounded_wait(aid, task, {'error_code': 'test_slots', 'next_at': (now + timedelta(seconds=2)).isoformat()}, now); return
            self.m._active_tests += 1
        try:
            result = self._request(row, 'reset', task, now)
            task = self.state(aid)
            if task.get('stage') == 'finished':
                return
            if task.get('consumed') and not result.get('uncertain'):
                self._test(self.m._read_account(aid), task, self.m.clock(), reserved_slot=True)
            elif task.get('attempt_at'):
                self._finish(aid, task, result.get('error_code') or 'result_uncertain', self.m.clock(), row)
            else:
                self._bounded_wait(aid, task, result, self.m.clock())
        finally:
            with self.m._test_slot_lock:
                self.m._active_tests -= 1

    def _undo_unissued_pause(self, aid, row, task, now):
        from .account_locks import control_lock
        if (not row or (not task.get('pause_undo_requested_at') and not self._owned(row, task))
                or not self._eligible(row, now, held=True)
                or task.get('policy_credential_fingerprint') != credential_fingerprint(row)):
            return
        with control_lock(self.m.db, aid):
            latest = self.state(aid)
            if latest.get('episode') != task['episode'] or latest.get('revision') != task.get('revision'):
                return
            actual = self.m._read_account(aid)
            if task.get('pause_undo_requested_at'):
                # Unknown writes are read back, never resent.
                if actual and actual.get('schedulable') is True and self.store.control_generation(aid) == task.get('control_generation'):
                    self._save(aid, task, owns_pause=False, pause_undo_confirmed_at=now.isoformat())
                return
            if not self._owned(actual, task) or not self._eligible(actual, now, held=True):
                return
            task = self._save(aid, task, pause_undo_requested_at=now.isoformat())
            self.schedule_runner(aid, True, **self._connection())
            actual = self.m._read_account(aid)
            if actual and actual.get('schedulable') is True and self.store.control_generation(aid) == task.get('control_generation'):
                self._save(aid, task, owns_pause=False, pause_undo_confirmed_at=self.m.clock().isoformat())

    def _bounded_wait(self, aid, task, result, now):
        if task.get('stage') == 'finished':
            return
        code = result.get('waiting_reason') or result.get('error_code') or 'incomplete_quota'
        until = parse_iso_datetime(result.get('next_at') or result.get('next_query_at'))
        if code == 'query_busy' and until is None:
            until = now + timedelta(seconds=2)
        end = parse_iso_datetime(task.get('verification_deadline_at') if task.get('consumed') else task.get('consume_deadline_at'))
        if (code in AUTH_ERRORS or code in {'no_credit', 'query_budget', 'account_ineligible', 'reset_state_changed'}
                or not until or not end or until >= end):
            self._finish(aid, task, code, now); return
        self._save(aid, task, next_at=until.isoformat(), error_code=code)

    def merge_disposition(self, aid, job_id, disposition, row):
        task = self.state(aid)
        if not task.get('policy_version') or task.get('model_test_job_id') != job_id:
            return False
        fields = dict(degradation_confirmed=bool(disposition.get('marked') and disposition.get('schedule_verified')),
                      model_id=disposition.get('model_id'), prediction=disposition.get('prediction'),
                      probability=disposition.get('probability'))
        if task.get('stage') != 'finished':
            self._finish(aid, task, 'detection_held', self.m.clock(), row,
                         cancel_verification=False, notice_fields=fields)
            return True
        def merge(data):
            notice = data['scheduler'][str(aid)].get('credit_result_notifications', {}).get(task['episode'])
            if notice and notice.get('status') == 'pending' and not notice.get('attempts'):
                notice.update(fields)
        self.store.transaction(merge)
        return True

    def deliver_results(self, notifier):
        """Bounded outbox. A reserved send counts even if the process stops."""
        if not self._notice_lock.acquire(blocking=False):
            return
        try:
            config = notifier.runtime_config()
            if not config.config_valid:
                return
            for aid, meta in self.store.snapshot()['scheduler'].items():
                for episode, notice in (meta.get('credit_result_notifications') or {}).items():
                    now = self.m.clock()
                    due = parse_iso_datetime(notice.get('next_at'))
                    if notice.get('status') != 'pending' or (due and due > now):
                        continue
                    old_task = meta.get('auto_reset_credit') or {}
                    if (old_task.get('episode') == episode and not old_task.get('attempt_at')
                            and not old_task.get('credit_admitted_at')):
                        self._migrate_credit_admission(int(aid), self.m._read_account(int(aid)), old_task, now)
                    def reserve(data):
                        item = data['scheduler'][aid]['credit_result_notifications'][episode]
                        if item != notice:
                            return None
                        if not config.enabled:
                            item.update(status='suppressed', next_at=None); return None
                        count = int(item.get('attempts') or 0)
                        if count >= 5:
                            item.update(status='failed', next_at=None); return None
                        item.update(attempts=count + 1, next_at=(now + timedelta(seconds=(5, 30, 120, 600, 600)[count])).isoformat())
                        return dict(item)
                    reserved = self.store.transaction(reserve)
                    if not reserved:
                        continue
                    try:
                        delivered = notifier.notify_credit_result(reserved, config=config)
                    except Exception:
                        delivered = False
                    def finish(data):
                        item = data['scheduler'][aid]['credit_result_notifications'][episode]
                        if item.get('attempts') != reserved['attempts']:
                            return
                        if delivered:
                            item.update(status='delivered', next_at=None)
                        elif reserved['attempts'] >= 5:
                            item.update(status='failed', next_at=None)
                    self.store.transaction(finish)
                    self._audit(int(aid), 'result_delivered' if delivered else 'result_retry', attempt=reserved['attempts'])
        finally:
            self._notice_lock.release()
