"""Durable, single-consumption recovery for an exhausted OpenAI OAuth parent.

No upstream request is made while discovering candidates. The reset endpoint's
implicit quota read is admitted by the same budget as an explicit quota read.
"""
from __future__ import annotations

import hashlib
import json
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timedelta
from threading import Lock
from typing import Any, Callable

from .audit import write_audit
from .auto_reset_policy import BoundedCreditPolicy, RESULT_LABELS, REASONS, verification_retryable
from .desktop_usage import reset_credits
from .key_fallback import execute_sub2api_set_schedulable
from .account_locks import AccountLease, control_lock
from .oauth_queries import AUTH_ERRORS, automatic_eligible, credential_fingerprint, fresh_quota, quota_complete
from .quota_snapshot import latest_openai_result
from .usage_query import (oauth_quota_from_usage_data, oauth_quota_summary_from_result,
                          oauth_windows_by_key, parse_iso_datetime, percent_or_none,
                          required_oauth_window_keys)
from .error_evidence import is_local_throttle, local_throttle_sql
from .reset_credit_observation import (available_credits, create_observation, digest, limit_fingerprint,
                                       normalized_credits, receipt_diagnostics, validate_observation, consumption_result)

STAGES = {"waiting", "pausing", "resetting", "uncertain", "testing", "retry", "confirming",
          "releasing", "recovered", "manual", "blocked", "closed", "finished"}
RETRY_SECONDS = (60, 300, 900, 1800)
STATE_LABELS = {"waiting": "等待用卡", "pausing": "暂停调度中", "resetting": "重置中",
                "uncertain": "重置待确认", "testing": "测活中", "retry": "等待重试测活",
                "confirming": "等待额度确认", "releasing": "恢复调度中",
                "recovered": "用卡恢复完成", "manual": "已转人工处理", "blocked": "自动用卡已暂停", "closed": "等待已结束"}
ERRORS = {"no_credit": "没有可用重置卡", "conflict": "Sub2API 自动用卡已开启，存在冲突",
          "nothing_to_reset": "上游确认当前无需重置，等待状态核对",
          "already_redeemed": "上游报告已兑换，本轮消费结果待确认",
          "ownership_changed": "账号已被其他操作修改，请人工确认调度状态",
          "pause_uncertain": "暂停结果待确认，未发送用卡请求", "auth_paused": "认证异常，等待凭据更新",
          "result_uncertain": "重置结果待确认，禁止重复用卡", "reset_state_changed": "账号或额度证据已变化",
          "query_state_unavailable": "状态无法保存，已暂停自动处理", "test_failed": "测活失败",
          "incomplete_quota": "额度证据不完整", "query_cooldown": "等待查询冷却",
          "query_budget": "等待查询预算", "query_backoff": "等待查询退避",
          "credit_snapshot_unconfirmed": "重置卡查询结果尚未确认，等待重新核对",
          "quota_unavailable": "必要额度窗口尚未恢复", "recovery_failed": "恢复调度未确认",
          "model_verification_pending": "等待模型验证", "model_verifier_unavailable": "模型验证服务尚未就绪",
          "query_slots": "等待额度查询执行槽", "test_slots": "等待验证执行槽",
          "model_cooldown": "模型验证冷却中", "model_busy": "等待当前模型测试结束"}
CREDIT_EVIDENCE_ERRORS = {"missing": "缺少可信卡观测", "content_changed": "卡快照内容已变化",
    "credentials_changed": "凭据已变化", "limit_changed": "限流事件已变化", "window_changed": "额度窗口已变化",
    "outside_window": "卡观测不在当前额度窗口内", "quota_missing": "缺少有效额度时间", "new_failure": "出现更新的查询失败证据"}


def validate_state(value: Any) -> None:
    if not isinstance(value, dict) or value.get("stage") not in STAGES or not value.get("episode"):
        raise ValueError("自动用卡状态无效")
    for key in ("attempt_at", "reset_at", "next_at", "test_completed_at", "recovered_at", "evidence_at",
                "reset_completed_at", "release_requested_at", "manual_at", "trigger_at", "consume_deadline_at",
                "verification_deadline_at", "completed_at", "policy_checked_at", "supplement_attempt_at",
                "clear_requested_at", "pause_undo_requested_at", "pause_undo_confirmed_at", "recovered_evidence_at"):
        if value.get(key) and parse_iso_datetime(value[key]) is None:
            raise ValueError("自动用卡时间无效")
    if value.get("owns_pause") not in (True, False, None):
        raise ValueError("自动用卡暂停归属无效")
    if value.get('policy_version') is not None:
        if type(value['policy_version']) is not int or value['policy_version'] != 1:
            raise ValueError('自动用卡策略版本无效')
        for key in ('test_attempts', 'policy_control_generation', 'policy_method_generation'):
            if key in value and (type(value[key]) is not int or value[key] < 0):
                raise ValueError('自动用卡控制状态无效')
        if value.get('outcome') is not None and value['outcome'] not in RESULT_LABELS:
            raise ValueError('自动用卡结果无效')
        version = value.get('policy_detection_version')
        if version is not None and (not isinstance(version, str) or len(version) != 64
                                    or any(char not in '0123456789abcdef' for char in version)):
            raise ValueError('自动用卡检测控制版本无效')


def project_state(value: dict[str, Any] | None, notices: dict | None = None) -> dict[str, Any] | None:
    if not value:
        return None
    if value.get('credit_gate_skipped') and not value.get('attempt_at'):
        if not value.get('owns_pause'):
            return None
        return {'stage': 'blocked', 'label': '调度暂停待核对',
                'error': '用卡已跳过；原临时暂停尚未确认解除', 'next_at': None,
                'evidence_source': value.get('evidence_source'), 'evidence_at': value.get('evidence_at')}
    if (value.get('stage') == 'manual' and value.get('attempt_at') and not value.get('reset_completed_at')
            and value.get('consumed') is not True and value.get('consumption_outcome') != 'not_consumed'):
        value = {**value, 'stage': 'uncertain', 'error_code': 'result_uncertain', 'next_at': None}
    if not value.get('policy_version') and value.get('stage') in {'manual', 'recovered', 'closed'}:
        return None
    code = str(value.get('result_reason') or value.get('error_code') or '')
    finished = value.get('stage') in {'finished', 'recovered'}
    notice = (notices or {}).get(value.get('episode'), {})
    return {'stage': value['stage'], 'label': RESULT_LABELS.get(value.get('outcome'), '最近用卡结果') if finished else STATE_LABELS.get(value['stage'], '用卡处理中'),
            'error': REASONS.get(code, ERRORS.get(code, '操作未确认' if code else '')),
            **{key: value.get(key) for key in ('trigger_at', 'consume_deadline_at', 'verification_deadline_at',
                'outcome', 'completed_at', 'next_at', 'attempt_at', 'evidence_source', 'evidence_at',
                'test_completed_at', 'recovered_at')},
            'notification_status': notice.get('status'), 'notification_attempts': notice.get('attempts', 0)}


def upstream_evidence(db: Any, row: dict[str, Any], now: datetime) -> dict[str, Any] | None:
    start = parse_iso_datetime(row.get("rate_limited_at"))
    reset = parse_iso_datetime(row.get("rate_limit_reset_at"))
    if not start or start > now or not reset or reset <= now or reset <= start:
        return None
    # WS semantic 429s persist this account block without necessarily emitting
    # an ops_error_logs row. Quota and eligibility are checked by _depletion.
    params = {"id": int(row["id"]), "start": start - timedelta(seconds=5), "now": now}
    enriched = f"""
        SELECT e.id,e.created_at,
               e.error_owner,e.error_phase,
               to_jsonb(e)->>'upstream_error_message' AS upstream_error_message,
               to_jsonb(e)->>'error_message' AS error_message,
               to_jsonb(e)->'error_body' AS error_body,
               to_jsonb(e)->'upstream_error_detail' AS upstream_error_detail,
               to_jsonb(e)->'upstream_errors' AS upstream_errors
        FROM ops_error_logs e
        WHERE e.account_id=%(id)s AND e.upstream_status_code=429
          AND coalesce(e.error_owner,'') <> 'client'
          AND NOT {local_throttle_sql()}
          AND e.created_at >= %(start)s AND e.created_at <= %(now)s
          AND NOT EXISTS (SELECT 1 FROM ops_error_logs x WHERE x.account_id=e.account_id
            AND x.created_at>e.created_at AND x.upstream_status_code IN (401,402))
        ORDER BY e.created_at ASC,e.id ASC LIMIT 1
    """
    try:
        candidate = db.fetch_one(enriched, params)
        if candidate and not is_local_throttle(candidate):
            return {**candidate, "source": "upstream_error"}
        # Missing logs are compatible; failed reads or affirmative contrary
        # evidence are not. Do not turn a local limiter into a consumption grant.
        blocked = db.fetch_one(f"""
            SELECT e.id FROM ops_error_logs e
            WHERE e.account_id=%(id)s AND e.created_at >= %(start)s AND e.created_at <= %(now)s
              AND (e.upstream_status_code IN (401,402) OR {local_throttle_sql()}
                   OR (e.error_owner='client' AND (e.status_code=429 OR e.upstream_status_code=429)))
            ORDER BY e.created_at DESC,e.id DESC LIMIT 1
        """, params)
    except Exception:
        raise ValueError("限流证据无法可靠读取") from None
    if blocked:
        return None
    fingerprint = hashlib.sha256(f"{row['id']}:{start.isoformat()}:{reset.isoformat()}".encode()).hexdigest()
    return {"id": f"account-rate-limit:{fingerprint}", "created_at": start,
            "source": "account_rate_limit"}


def _signature(row: dict[str, Any]) -> str:
    credentials = row.get("credentials") or {}
    stable = {key: row.get(key) for key in ("id", "name", "platform", "type", "parent_account_id",
              "account_priority", "priority", "concurrency", "expires_at", "auto_pause_on_expired")}
    stable["identity"] = {key: credentials.get(key) for key in ("plan_type", "chatgpt_account_id", "email")}
    # Preserve configuration ownership; exclude only runtime quota snapshots.
    stable["extra"] = {k: v for k, v in (row.get("extra") or {}).items()
                       if not k.startswith(("codex_5h_", "codex_7d_", "codex_usage_", "codex_reset_credit_", "codex_credits_"))}
    return hashlib.sha256(json.dumps(stable, sort_keys=True, default=str).encode()).hexdigest()


def _parameter_edit_guard(row: dict[str, Any]) -> str:
    # A narrowly authorized scalar edit must not adopt concurrent manual
    # scheduling, credential or blocking changes. Only hashes leave memory.
    protected = [_signature(row), row.get("credentials"),
                 {k: row.get(k) for k in ("status", "schedulable", "load_factor", "proxy_id", "rate_limited_at",
                  "rate_limit_reset_at", "overload_until", "temp_unschedulable_until", "temp_unschedulable_reason")}]
    return hashlib.sha256(json.dumps(protected, sort_keys=True, default=str).encode()).hexdigest()


def quota_result(data: dict[str, Any], row: dict[str, Any], now: datetime, *, requested_at: datetime | None = None) -> dict[str, Any]:
    payload = data.get("quota") if isinstance(data.get("quota"), dict) else data
    windows = payload.get("rate_limit") or {}
    normal: dict[str, Any] = {}
    for value in (windows.get("primary_window"), windows.get("secondary_window")):
        if not isinstance(value, dict):
            continue
        duration = value.get("limit_window_seconds")
        # Free plans can expose a longer primary quota window (for example 30d).
        # Preserve the actual boundary instead of guessing a seven-day reset.
        key = "five_hour" if duration == 18000 else "seven_day" if isinstance(duration, (int, float)) and duration > 18000 else None
        if key:
            normal[key] = {"utilization": value.get("used_percent"), "resets_at": value.get("reset_at"),
                           "remaining_seconds": value.get("reset_after_seconds"), "window_minutes": duration / 60}
    # Older compatible servers may already use the admin usage representation.
    if not windows:
        normal = {k: payload[k] for k in ("five_hour", "seven_day") if isinstance(payload.get(k), dict)}
    observed = parse_iso_datetime(payload.get("fetched_at")) if payload.get("fetched_at") is not None else now
    valid_time = observed is not None and observed <= now
    result = {"account_id": int(row["id"]), "template_type": "oauth", "success": bool(normal) and valid_time,
            "source": "sub2api_admin_usage", "queried_at": (observed or now).isoformat(),
            "oauth_quota": oauth_quota_from_usage_data(normal, row, now=observed or now)}
    if requested_at and observed and requested_at.replace(microsecond=0) <= observed <= now:
        # Keep the upstream timestamp. The request interval only proves that a
        # seconds-resolution snapshot came from this active read, not an old cache.
        result.update(read_started_at=requested_at.isoformat(), read_completed_at=now.isoformat())
    return result


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def execute_credit_request(action: str, account_id: int, *, base_url: str, admin_token: str,
                           timeout_seconds: int = 90, urlopen: Callable | None = None) -> dict[str, Any]:
    path = "reset-quota" if action == "reset" else "quota/refresh"
    request = urllib.request.Request(f"{base_url.rstrip('/')}/api/v1/admin/openai/accounts/{account_id}/{path}",
        data=b"{}", method="POST", headers={"x-api-key": admin_token, "Accept": "application/json", "Content-Type": "application/json"})
    diagnostics = receipt_diagnostics(None, None)
    try:
        with (urlopen or urllib.request.build_opener(_NoRedirect()).open)(request, timeout=timeout_seconds) as response:
            status = getattr(response, "status", None)
            diagnostics = receipt_diagnostics(status, None)
            body = json.loads(response.read(2_000_000))
        data = body.get("data") if isinstance(body, dict) else None
        envelope_ok = isinstance(body, dict) and type(body.get("code")) is int and body["code"] == 0
        diagnostics = receipt_diagnostics(status, data if envelope_ok and isinstance(data, dict) else body)
        if (not envelope_ok or not isinstance(data, dict)
                or (status is not None and not 200 <= status < 300)):
            return {"success": False, "uncertain": action == "reset", "error_code": "result_uncertain",
                    "diagnostics": diagnostics}
        outcome = consumption_result(data) if action == "reset" else {
            "success": True, "consumed": False, "uncertain": False, "error_code": ""}
        return {**outcome, "data": data, "diagnostics": diagnostics}
    except urllib.error.HTTPError as exc:
        try:
            error_body = json.loads(exc.read(2_000_000))
            error_data = error_body.get("data") if isinstance(error_body, dict) else None
            diagnostics = receipt_diagnostics(exc.code, error_data if isinstance(error_data, dict) else error_body)
        except Exception:
            diagnostics = receipt_diagnostics(exc.code, None)
        return {"success": False, "uncertain": action == "reset", "error_code": f"http_{exc.code}",
                "diagnostics": diagnostics}
    except Exception:
        return {"success": False, "uncertain": action == "reset", "error_code": "result_uncertain",
                "diagnostics": diagnostics}


class AutoResetController(BoundedCreditPolicy):
    def __init__(self, monitor: Any, *, request_runner: Callable = execute_credit_request,
                 schedule_runner: Callable = execute_sub2api_set_schedulable,
                 evidence_reader: Callable = upstream_evidence) -> None:
        self.m = monitor
        self.store = monitor.store
        self.request_runner, self.schedule_runner, self.evidence_reader = request_runner, schedule_runner, evidence_reader
        self._notice_lock = Lock()

    def state(self, account_id: int) -> dict[str, Any]:
        return dict(self.store.snapshot()["scheduler"].get(str(account_id), {}).get("auto_reset_credit") or {})

    def _save(self, account_id: int, task: dict[str, Any], **changes: Any) -> dict[str, Any]:
        updated = {**task, **changes, "revision": int(task.get("revision") or 0) + 1}
        validate_state(updated)
        def save(data):
            meta = data["scheduler"].setdefault(str(account_id), {})
            current = meta.get("auto_reset_credit") or {}
            if (current.get("episode") not in (None, task.get("episode"))
                    or int(current.get("revision") or 0) != int(task.get("revision") or 0)):
                raise ValueError("自动用卡任务已变化")
            checked = max(filter(None, (parse_iso_datetime(current.get('policy_checked_at')), parse_iso_datetime(updated.get('policy_checked_at')))), default=None)
            if checked:
                updated['policy_checked_at'] = checked.isoformat()
            if 'model_test_job_id' not in changes and current.get('model_test_job_id'):
                updated['model_test_job_id'] = current['model_test_job_id']
            meta["auto_reset_credit"] = updated
        self.store.transaction(save)
        return updated

    def _audit(self, account_id: int, action: str, **fields: Any) -> None:
        write_audit(self.m.settings.audit_path, "oauth_auto_reset_credit", {"account_id": account_id, "action": action, **fields})

    def _begin_episode(self, aid: int, old: dict[str, Any], evidence: dict[str, Any]) -> dict[str, Any]:
        row = self.m._read_account(aid) or {}
        now = self.m.clock()
        evidence = self._admit_credit_episode(row, evidence, now)
        if not evidence:
            return {}
        task = {"episode": uuid.uuid4().hex, "stage": "waiting", "account_name": str(row.get("name") or ""),
                **evidence, 'credit_admitted_at': now.isoformat(),
                'credit_admitted_count': available_credits((row.get('extra') or {}).get('codex_reset_credit_snapshot'), now)}
        def replace(data):
            meta = data["scheduler"].setdefault(str(aid), {})
            current = meta.get("auto_reset_credit") or {}
            if current.get("episode") != old.get("episode") or current.get("revision") != old.get("revision"):
                raise ValueError("自动用卡任务已变化")
            if old.get("attempt_at"):
                meta.setdefault("reset_credit_episodes", {})[old["episode"]] = dict(old)
            meta["auto_reset_credit"] = task
        self.store.transaction(replace)
        return task

    def cancel(self, account_id: int) -> None:
        task = self.state(account_id)
        if task and task.get("stage") != "recovered":
            self._save(account_id, task, stage="manual", owns_pause=False, next_at=None, error_code="",
                       manual_at=self.m.clock().isoformat())
            self._audit(account_id, "manual_intervention")

    def prepare_parameter_edit(self, account_id: int, field: str, value: int | None) -> None:
        """Called under the account lease, durably before the scalar write."""
        if field not in {"priority", "concurrency", "load_factor"}:
            raise ValueError("不是普通调度参数")
        row = self.m._read_account(account_id)
        task = self._reconcile_parameter_edit(row, self.state(account_id))
        if not task.get("owns_pause"):
            return
        if not self._owned(row, task):
            raise ValueError("用卡暂停归属待核对，未修改参数")
        desired = dict(row)
        if field == "priority":
            for key in ("account_priority", "priority"):
                if key in desired:
                    desired[key] = value
        else:
            desired[field] = value
        self._save(account_id, task, parameter_edit={
            "field": field, "target": value, "guard": _parameter_edit_guard(desired),
            "generation": self.store.control_generation(account_id),
        })

    def _reconcile_parameter_edit(self, row: dict | None, task: dict) -> dict:
        pending = task.get("parameter_edit")
        if not row or not isinstance(pending, dict) or not task.get("owns_pause"):
            return task
        aid = int(row["id"])
        if (pending.get("generation") != self.store.control_generation(aid)
                or row.get("schedulable") is not False):
            return task
        if pending.get("guard") == _parameter_edit_guard(row):
            return self._save(aid, task, signature=_signature(row),
                              owned_version=str(row.get("updated_at")), parameter_edit=None)
        if self._owned(row, task):
            # The write was never dispatched (or was rejected); keep the old hold.
            return self._save(aid, task, parameter_edit=None)
        return task

    def reconcile_parameter_edit(self, account_id: int) -> None:
        self._reconcile_parameter_edit(self.m._read_account(account_id), self.state(account_id))

    def _saved_quota(self, row, now):
        saved = self.store.snapshot()["oauth_results"].get(str(row["id"]))
        return latest_openai_result(row, saved, now)

    def _credit_known(self, row, evidence, now):
        return not self._credit_reason(row, evidence, now)

    def _credit_reason(self, row, evidence, now):
        """Reuse a verified observation, never date an old native cache as now."""
        if not parse_iso_datetime(evidence.get('reset_at')) or not evidence.get('window_minutes'):
            return 'outside_window'
        meta = self.store.snapshot()["scheduler"].get(str(row["id"]), {})
        observation = meta.get("reset_credit_observation")
        raw = (row.get("extra") or {}).get("codex_reset_credit_snapshot") or {}
        if normalized_credits(raw) is None:
            return 'invalid_credit_snapshot'
        if observation is not None:
            validate_observation(observation)
            content = normalized_credits(raw)
            if content is None or digest(content) != observation["snapshot_sha256"]:
                return "content_changed"
            if credential_fingerprint(row) != observation["credential_fingerprint"]:
                return "credentials_changed"
            if limit_fingerprint(row) != observation["limit_fingerprint"]:
                return "limit_changed"
            if (parse_iso_datetime(observation["window_reset_at"]) != parse_iso_datetime(evidence["reset_at"])
                    or observation["window_minutes"] != evidence["window_minutes"]):
                return "window_changed"
            observed = parse_iso_datetime(observation["observed_at"])
        else:
            # Compatibility for older servers that explicitly dated their card
            # cache. A native undated snapshot must first be actively verified.
            observed = reset_credits(row.get("extra") or {}, now)["observed_at"]
        reset = parse_iso_datetime(evidence["reset_at"])
        if not observed:
            return "missing"
        if not reset or not reset - timedelta(minutes=evidence["window_minutes"]) <= observed <= now or observed >= reset:
            return "outside_window"
        return "" if parse_iso_datetime((self._saved_quota(row, now) or {}).get("queried_at")) else "quota_missing"

    def _credit_observation(self, before, after, result, started, completed):
        payload = result.get("data") or {}
        if (not result.get("success") or payload.get("cache_persisted") is not True or not after
                or credential_fingerprint(before) != credential_fingerprint(after)
                or limit_fingerprint(before) != limit_fingerprint(after)):
            return None
        content = normalized_credits(payload.get("rate_limit_reset_credits"))
        actual = normalized_credits((after.get("extra") or {}).get("codex_reset_credit_snapshot"))
        observed = parse_iso_datetime(payload.get("fetched_at")) if payload.get("fetched_at") is not None else completed
        if (content is None or content != actual or not observed
                or observed < started - timedelta(seconds=1) or observed > completed):
            return None
        summary = oauth_quota_summary_from_result(after, result.get("quota_result"))
        window = oauth_windows_by_key(summary.get("ui_windows")).get("codex_7d", {})
        reset = parse_iso_datetime(window.get("reset_at"))
        if not reset or reset <= observed:
            return None
        return create_observation(after, content, observed, credential_fingerprint(after), reset,
                                  window.get("window_minutes") or 7 * 24 * 60)

    def _eligible(self, row, now, *, held=False):
        from .recovery_policy import recovery_method
        if not row or not recovery_method(self.m.settings, int(row['id'])) or not getattr(self.m.settings, 'oauth_recovery_monitor_enabled', True):
            return False
        return bool(row and not getattr(self.m, "detection_gate", lambda _aid: False)(int(row["id"])) and not row.get("parent_account_id")
                    and not (row.get("extra") or {}).get("parent_account_id")
                    and automatic_eligible({**row, "schedulable": True} if held else row, now))

    def _depletion(self, row, now, *, held=False):
        if not self._eligible(row, now, held=held):
            return None
        block = parse_iso_datetime(row.get("rate_limit_reset_at"))
        if not block or block <= now:
            return None
        metadata = self.store.snapshot()["scheduler"].get(str(row["id"]), {})
        query = metadata.get("quota_query") or {}
        if query.get("auth_fingerprint") == credential_fingerprint(row) or (
                not query.get("auth_fingerprint") and metadata.get("last_error_code") in AUTH_ERRORS):
            return None
        saved = self._saved_quota(row, now)
        observed = parse_iso_datetime((saved or {}).get("queried_at"))
        if not saved or not saved.get("success") or not observed or observed > now:
            return None
        summary = oauth_quota_summary_from_result(row, saved)
        seven = oauth_windows_by_key(summary.get("ui_windows")).get("codex_7d", {})
        used, reset = percent_or_none(seven.get("used_percent")), parse_iso_datetime(seven.get("reset_at"))
        if used is None or used < 100 or not reset or reset <= now or observed >= reset:
            return None
        evidence = self.evidence_reader(self.m.db, row, now)
        stamp = parse_iso_datetime((evidence or {}).get("created_at"))
        if not stamp or stamp > now:
            return None
        duration = seven.get("window_minutes") or 7 * 24 * 60
        if observed < reset - timedelta(minutes=duration) or stamp < reset - timedelta(minutes=duration):
            return None
        fingerprint = hashlib.sha256(json.dumps({"account_id": row["id"],
            "limited_at": str(row.get("rate_limited_at")), "block_until": block.isoformat(),
            "quota_reset": reset.isoformat()}, sort_keys=True).encode()).hexdigest()
        return {"reset_at": reset.isoformat(), "window_minutes": duration,
                "evidence_at": stamp.isoformat(), "evidence_id": str(evidence["id"]),
                "evidence_source": evidence.get("source", "upstream_error"), "evidence_fingerprint": fingerprint}

    def _available(self, row, now, task):
        result = self._saved_quota(row, now)
        observed = parse_iso_datetime((result or {}).get("queried_at"))
        attempted = parse_iso_datetime(task.get("attempt_at"))
        if not fresh_quota(row, result, now) or not observed:
            return False
        if attempted and observed < attempted:
            started = parse_iso_datetime((result or {}).get("read_started_at"))
            completed = parse_iso_datetime((result or {}).get("read_completed_at"))
            if not (started and completed and attempted <= started <= completed <= now
                    and started.replace(microsecond=0) <= observed <= completed):
                return False
        summary = oauth_quota_summary_from_result(row, result)
        windows = oauth_windows_by_key(summary.get("ui_windows"))
        return all((percent_or_none(windows.get(k, {}).get("used_percent")) or 0) < 100
                   for k in required_oauth_window_keys(summary.get("plan_type")))

    def _processing_eligible(self, row, task, now):
        # A test may itself mark a 401 account as error. Only changed
        # credentials authorize re-testing that exact auth error under our hold.
        candidate = row
        if (row and row.get("status") == "error" and task.get("auth_recheck")
                and task.get("auth_error") == hashlib.sha256(str(row.get("error_message") or "").encode()).hexdigest()):
            candidate = {**row, "status": "active"}
        return self._eligible(candidate, now, held=True)

    def _owned(self, row, task, *, same_version=True):
        return bool(row and task.get("owns_pause") and row.get("schedulable") is False
                    and _signature(row) == task.get("signature")
                    and (task.get("control_generation") is None
                         or self.store.control_generation(int(row["id"])) == task["control_generation"])
                    and (not same_version or str(row.get("updated_at")) == task.get("owned_version")))

    def _connection(self):
        return {"base_url": self.m.base_url_provider(), "admin_token": self.store.admin_token()}

    def _request(self, row, action, task, now, source="automatic", token=None, request_runner=None):
        aid = int(row["id"])
        connection = self._connection()
        if token:
            connection["admin_token"] = token
        def valid(data):
            current = data["scheduler"].get(str(aid), {}).get("auto_reset_credit") or {}
            return (current.get("episode") == task["episode"] and current.get("stage") == task["stage"]
                    and current.get("revision") == task.get("revision"))
        def operation():
            live = self.m._read_account(aid)
            if not live:
                return {"success": False, "error_code": "reset_state_changed"}
            active_task = task
            if source == 'automatic' and task.get('policy_version'):
                reason = self._guard_reason(aid, live, task, self.m.clock())
                if reason:
                    return {'success': False, 'error_code': reason}
                if self._expired(task, self.m.clock()) or task.get('stage') == 'finished':
                    return {'success': False, 'error_code': 'consume_deadline'}
                if action == 'query':
                    if task.get('supplement_attempt_at'):
                        return {'success': False, 'error_code': 'supplement_limit'}
                    active_task = self._save(aid, task, supplement_attempt_at=self.m.clock().isoformat())
            if source == "automatic":
                if (live.get("extra") or {}).get("auto_reset_credit_enabled") is not False and (live.get("extra") or {}).get("auto_reset_credit_enabled") is not None:
                    return {"success": False, "error_code": "conflict"}
                if action == "reset":
                    # Re-read all evidence after budget admission, immediately
                    # before pausing and issuing the one consumption request.
                    depletion = self._depletion(live, now)
                    if (not depletion or depletion["reset_at"] != task.get("reset_at")
                            or depletion["evidence_fingerprint"] != task.get("evidence_fingerprint")):
                        return {"success": False, "error_code": "reset_state_changed"}
                    credit_reason = self._positive_credit_reason(live, depletion, self.m.clock())
                    if credit_reason:
                        return {"success": False, "error_code": credit_reason}
                    with control_lock(self.m.db, aid):
                        active_task = self._save(aid, task, stage="pausing", signature=_signature(live), next_at=None,
                                                control_generation=self.store.control_generation(aid))
                        paused = self.schedule_runner(aid, False, **connection)
                        after = self.m._read_account(aid)
                        latest = self.state(aid)
                        if latest.get('stage') == 'finished' and latest.get('episode') == active_task['episode']:
                            if (after and after.get('schedulable') is False
                                    and _signature(after) == active_task['signature']
                                    and self.store.control_generation(aid) == active_task.get('control_generation')):
                                self._save(aid, latest, owns_pause=True, owned_version=str(after.get('updated_at')))
                            return {'success': False, 'error_code': 'consume_deadline'}
                        if (not paused.get("success") or not after or after.get("schedulable") is not False
                                or _signature(after) != active_task["signature"]):
                            self._save(aid, active_task, stage="blocked", error_code="pause_uncertain")
                            return {"success": False, "error_code": "pause_uncertain"}
                        active_task = self._save(aid, active_task, stage="resetting", owns_pause=True,
                                                owned_version=str(after.get("updated_at")))
                    # A second live read protects changes made during the pause.
                    final = self.m._read_account(aid)
                    final_depletion = self._depletion(final, self.m.clock(), held=True) if final else None
                    if (not self._owned(final, active_task) or not final_depletion
                            or final_depletion["reset_at"] != task.get("reset_at")
                            or final_depletion["evidence_fingerprint"] != task.get("evidence_fingerprint")):
                        self._save(aid, active_task, stage="blocked", error_code="ownership_changed")
                        return {"success": False, "error_code": "ownership_changed"}
                    credit_reason = self._positive_credit_reason(final, final_depletion, self.m.clock())
                    if credit_reason:
                        self._save(aid, active_task, stage="blocked", error_code=credit_reason)
                        return {"success": False, "error_code": credit_reason}
                elif task.get("owns_pause") and not self._owned(live, task):
                    return {"success": False, "error_code": "ownership_changed"}
                elif not task.get("owns_pause") and not self._depletion(live, now):
                    return {"success": False, "error_code": "reset_state_changed"}
            elif action == "reset":
                active_task = self._save(aid, task, stage="uncertain", attempt_at=self.m.clock().isoformat())
            if self.state(aid).get("revision") != active_task.get("revision"):
                return {"success": False, "error_code": "manual_intervention"}
            requested_at = self.m.clock()
            if source == 'automatic' and action == 'reset':
                with control_lock(self.m.db, aid):
                    requested_at = self.m.clock()
                    reason = self._guard_reason(aid, self.m._read_account(aid), active_task, requested_at)
                    if reason:
                        return {'success': False, 'error_code': reason}
                    if self._expired(active_task, requested_at):
                        return {'success': False, 'error_code': 'consume_deadline'}
                    active_task = self._save(aid, active_task, attempt_at=requested_at.isoformat())
            result = (request_runner or self.request_runner)(action, aid, **connection)
            completed = self.m.clock()
            diagnostics = result.get("diagnostics") or receipt_diagnostics(None, result.get("data"))
            # Persist confirmed consumption before the coordinator's quota write;
            # a failed latter write cannot turn success into a repeatable request.
            if action == "reset":
                outcome = result.get("consumption_outcome") or ("consumed" if result.get("consumed") else "unknown")
                receipt_code = "" if result.get("consumed") else str(result.get("error_code") or "result_uncertain")
                latest = self.state(aid)
                if latest.get("episode") == active_task.get("episode") and latest.get("stage") in {"manual", "finished"}:
                    # A scheduling intervention revokes recovery authority, not
                    # the irreversible consumption receipt from an in-flight call.
                    self._save(aid, latest, consumed=bool(result.get("consumed")),
                               reset_completed_at=completed.isoformat() if result.get("consumed") else None,
                               error_code=receipt_code if latest.get('stage') != 'finished' else latest.get('error_code', ''),
                               consumption_outcome=outcome, receipt=diagnostics)
                    if latest.get('stage') == 'finished':
                        def amend(data):
                            value = data['scheduler'][str(aid)]['auto_reset_credit']
                            value['outcome'] = self._result(value)
                            notice = data['scheduler'][str(aid)].get('credit_result_notifications', {}).get(value['episode'])
                            if notice and notice.get('status') == 'pending' and not notice.get('attempts'):
                                notice.update(outcome=value['outcome'], reset_completed_at=value.get('reset_completed_at'))
                        self.store.transaction(amend)
                    return result
                stage = ("manual" if source != "automatic" and outcome != "unknown" else
                         "testing" if result.get("consumed") else "confirming" if outcome == "not_consumed" else "uncertain")
                active_task = self._save(aid, active_task, stage=stage,
                    consumed=bool(result.get("consumed")), reset_completed_at=completed.isoformat() if result.get("consumed") else None,
                    consumption_outcome=outcome, error_code=receipt_code, receipt=diagnostics,
                    verification_deadline_at=(completed + timedelta(seconds=300)).isoformat() if result.get("consumed") and source == "automatic" else None)
            if isinstance(result.get("data"), dict):
                try:
                    result["quota_result"] = quota_result(result["data"], live, completed, requested_at=requested_at)
                except (ValueError, TypeError, AttributeError, OverflowError):
                    result["quota_result"] = {"success": False, "error_code": "incomplete_quota"}
                if action == "query" and not quota_complete(live, result["quota_result"]):
                    result.update(success=False, error_code="incomplete_quota")
            after = self.m._read_account(aid)
            if action == "query":
                observation = self._credit_observation(live, after, result, now, completed)
                if observation is not None:
                    result["reset_credit_observation"] = observation
                elif result.get("success"):
                    # Explicitly dated legacy caches remain compatible, but do
                    # not persist a new observation without a matched receipt.
                    stamp = reset_credits((after or {}).get("extra") or {}, completed)["observed_at"]
                    legacy = ("rate_limit_reset_credits" not in (result.get("data") or {})
                              and (result.get("data") or {}).get("cache_persisted") is not False
                              and stamp and now - timedelta(seconds=1) <= stamp <= completed)
                    if not legacy:
                        result.update(success=False, error_code="credit_snapshot_unconfirmed")
            if active_task.get("owns_pause") and self._owned(after, active_task, same_version=False):
                self._save(aid, active_task, owned_version=str(after.get("updated_at")))
            return result
        result = self.m.queries.external_read(row, source=source, reason="reset_credit_consume" if action == "reset" else "reset_credit_check",
                                             operation=operation, validate=valid, now=now,
                                             bypass_interval=source == "automatic" and action == "reset",
                                             required_budget=2 if source == "automatic" and action == "query" else 1)
        if source == 'automatic' and action == 'reset' and result.get('error_code') == 'result_uncertain':
            latest = self.state(aid)
            # Consumption cannot leave the process before its durable attempt.
            # A failure while saving the preceding pause is not an unknown debit.
            if latest.get('episode') == task.get('episode') and not latest.get('attempt_at'):
                result = {**result, 'uncertain': False, 'error_code':
                          'pause_uncertain' if latest.get('stage') == 'pausing' else 'query_state_unavailable'}
        self._audit(aid, action, source=source, success=bool(result.get("success")),
                    error_code=result.get("error_code", ""), next_at=result.get("next_query_at"),
                    diagnostics=result.get("diagnostics", {}))
        return result

    def run(self, rows: list[dict[str, Any]], now: datetime) -> None:
        if (not getattr(self.m.settings, 'oauth_auto_reset_credit_enabled', False)
                and not any(meta.get('auto_reset_credit') for meta in self.store.cached_snapshot()['scheduler'].values())):
            return
        self.sweep(rows, now)
        for item in rows:
            self.run_account(item, now)

    def run_account(self, item: dict[str, Any], now: datetime) -> None:
        """One bounded account step; waits never serialize unrelated accounts."""
        enabled = bool(getattr(self.m.settings, "oauth_auto_reset_credit_enabled", False))
        aid = int(item['id'])
        if not enabled and not self.state(aid).get('policy_version'):
            return
        try:
            with self.store.disk_lock(f"credit-operation-{aid}", blocking=False):
                if not self.credit_candidate_ready(self.m._read_account(aid) or item, now):
                    return
                lease = AccountLease(self.m.db, item)
                if not lease.acquire():
                    return
                try:
                    self._step(aid, now, enabled)
                except (OSError, ValueError, TypeError):
                    self._audit(aid, "paused", error_code="query_state_unavailable")
                except Exception:
                    self._audit(aid, "paused", error_code="state_check_failed")
                finally:
                    lease.release()
        except BlockingIOError:
            return

    def _step(self, aid, now, enabled):
        self._bounded_step(aid, now, enabled)

    def _defer(self, aid, task, result, now):
        self._bounded_wait(aid, task, result, now)

    def _test(self, row, task, now, *, reserved_slot=False):
        aid = int(row["id"])
        if task.get('policy_version'):
            reason = self._guard_reason(aid, row, task, now)
            if reason:
                self._finish(aid, task, reason, now, row)
                return
        if task.get('policy_version') and (task.get('stage') == 'finished' or self._expired(task, now)):
            self._finish(aid, task, 'verification_deadline', now, row)
            return
        if not self._owned(row, task) or not self._processing_eligible(row, task, now):
            self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
            return
        model = str(getattr(self.m.settings, "oauth_recovery_test_model_id", "gpt-5.6-luna"))
        attempts = int(task.get("test_attempts") or 0) + 1
        task = self._save(aid, task, stage="testing", verification_dispatched=True, test_attempts=attempts,
                          next_at=(now + timedelta(seconds=RETRY_SECONDS[min(attempts - 1, 3)])).isoformat())
        generation = self.store.control_generation(aid)
        from .recovery_policy import recovery_method
        method = recovery_method(self.m.settings, aid)
        selection = int(self.store.snapshot()['scheduler'].get(str(aid), {}).get('recovery_method_generation', 0))
        if method == 'model':
            verifier = getattr(self.m, 'model_verifier', None)
            test = (verifier(aid, kind='credit', fingerprint=task['episode'], attempt=attempts) if verifier else
                    {'success': False, 'error_code': 'model_verifier_unavailable'})
        elif method == 'connection':
            remaining = max(1, (parse_iso_datetime(task.get('verification_deadline_at')) - now).total_seconds()) if task.get('policy_version') else 30
            runner = self.m._timed_test_owned if reserved_slot else self.m._timed_test
            test = runner(aid, model, control_generation=generation, timeout_seconds=min(30, remaining), **self._connection())
        else:
            return
        if (self.store.control_generation(aid) != generation or method != recovery_method(self.m.settings, aid)
                or selection != int(self.store.snapshot()['scheduler'].get(str(aid), {}).get('recovery_method_generation', 0))):
            return
        current = self.state(aid)
        if current.get('stage') == 'finished' or current.get('revision') != task.get('revision'):
            return
        if test.get('deferred'):
            task = self._save(aid, task, stage='retry', verification_dispatched=False, test_attempts=attempts - 1,
                       next_at=test.get('next_at') or (now + timedelta(seconds=5)).isoformat(),
                       model_test_job_id=test.get('job_id'), error_code=test.get('waiting_reason') or test.get('error_code') or 'model_verification_pending')
            self._bounded_wait(aid, task, test, self.m.clock())
            return
        model = test.get('model_id') or model
        self.m._cycle_tests[aid] = test
        completed = self.m.clock()
        self._audit(aid, "test", success=bool(test.get("success")), error_code=str(test.get("error_code") or ""))
        if self.store.control_generation(aid) != generation:
            return
        live = self.m._read_account(aid)
        code = str(test.get("error_code") or "test_failed")
        auth_error = code in AUTH_ERRORS | {'http_403'} and not test.get("success")
        candidate = {**live, "status": "active"} if live and auth_error and live.get("status") == "error" else live
        if not self._owned(live, task, same_version=False) or not self._processing_eligible(candidate, task, completed):
            self._save(aid, task, stage="blocked", owns_pause=False, error_code="ownership_changed")
            return
        task = self._save(aid, task, owned_version=str(live.get("updated_at")), test_completed_at=completed.isoformat(),
                          test_success=bool(test.get("success")), verification_dispatched=False, model_id=model,
                          verification_method=method, verification_generation=selection)
        if task.get('policy_version') and self._expired(task, completed):
            self._finish(aid, task, 'verification_deadline', completed, live)
            return
        if not test.get("success"):
            if task.get('policy_version') and (attempts >= 2 or not verification_retryable(test, method)):
                self._finish(aid, task, code if attempts < 2 else 'verification_limit', completed, live)
                return
            retry_at = completed + timedelta(seconds=60)
            if task.get('policy_version') and retry_at >= parse_iso_datetime(task['verification_deadline_at']):
                self._finish(aid, task, 'verification_deadline', completed, live)
                return
            self._save(aid, task, stage="retry", error_code="auth_paused" if code in AUTH_ERRORS else "test_failed",
                       auth_fingerprint=credential_fingerprint(live) if code in AUTH_ERRORS else None,
                       auth_recheck=False,
                       auth_error=hashlib.sha256(str(live.get("error_message") or "").encode()).hexdigest() if code in AUTH_ERRORS else None,
                       next_at=None if code in AUTH_ERRORS else retry_at.isoformat())
            return
        if not self._available(live, completed, task):
            self._finish(aid, task, "incomplete_quota", completed, live)
            return
        self._release(live, task, completed)

    def _release(self, row, task, now):
        with control_lock(self.m.db, int(row["id"])):
            current = self.state(int(row["id"]))
            if current.get("episode") != task.get("episode") or current.get("revision") != task.get("revision"):
                return
            self._release_locked(row, task, now)

    def _release_locked(self, row, task, now):
        from .oauth_monitor import account_recovery_confirmed
        aid = int(row["id"])
        if task.get('policy_version'):
            reason = self._guard_reason(aid, row, task, now)
            if reason:
                self._finish(aid, task, reason, now, row)
                return
        from .recovery_policy import recovery_method
        if (task.get('verification_method') and task['verification_method'] != recovery_method(self.m.settings, aid)
                or task.get('verification_generation', 0) != int(self.store.snapshot()['scheduler'].get(str(aid), {}).get('recovery_method_generation', 0))):
            return
        if task.get('policy_version') and self._expired(task, now):
            self._finish(aid, task, 'verification_deadline', now, row)
            return
        fresh = self.m._read_account(aid)
        if (not self._owned(fresh, task, same_version=not bool(task.get('clear_requested_at')))
                or not self._processing_eligible(fresh, task, now)
                or not self._available(fresh, now, task)):
            self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
            return
        # Clearing an old block is allowed only after a successful test and fresh
        # available quota. Do not overwrite a newer rate-limit or manual pause.
        for key in ("rate_limited_at", "temp_unschedulable_until", "overload_until"):
            stamp = parse_iso_datetime(fresh.get(key))
            if stamp and stamp > (parse_iso_datetime(task.get("attempt_at")) or now):
                self._finish(aid, task, 'quota_unavailable', now, fresh)
                return
        if not task.get('clear_requested_at'):
            task = self._save(aid, task, clear_requested_at=self.m.clock().isoformat())
            self.m.recovery_runner(aid, **self._connection())
        cleared = self.m._read_account(aid)
        if (not self._owned(cleared, task, same_version=False)
                or not account_recovery_confirmed({**(cleared or {}), "schedulable": True})):
            # An uncertain administrative write is reconciled from account
            # state. It is never dispatched a second time for this episode.
            self._save(aid, task, stage="confirming", error_code="recovery_failed", next_at=(now + timedelta(seconds=2)).isoformat())
            return
        task = self._save(aid, task, stage="releasing", owned_version=str(cleared.get("updated_at")), next_at=None,
                          release_requested_at=self.m.clock().isoformat(),
                          release_control_generation=self.store.control_generation(aid),
                          release_credential_fingerprint=credential_fingerprint(cleared))
        verified = self.m._read_account(aid)
        if not self._owned(verified, task) or not self._processing_eligible(verified, task, self.m.clock()):
            self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
            return
        if not self._available(verified, self.m.clock(), task):
            self._finish(aid, task, 'incomplete_quota', self.m.clock(), verified)
            return
        if task.get('policy_version') and self._expired(task, self.m.clock()):
            self._finish(aid, task, 'verification_deadline', self.m.clock(), verified)
            return
        self.schedule_runner(aid, True, **self._connection())
        final = self.m._read_account(aid)
        if not self._release_confirmed(final, task, self.m.clock()):
            # The server may have completed a scheduling write after the client
            # timed out. Keep its intent and reconcile reads, never replay it.
            self._save(aid, task, error_code="recovery_failed", next_at=(self.m.clock() + timedelta(seconds=30)).isoformat())
            return
        self._finish_release(final, task)

    def _release_confirmed(self, row, task, now):
        from .oauth_monitor import account_recovery_confirmed
        from .recovery_policy import recovery_method
        if not row:
            return False
        aid = int(row["id"])
        metadata = self.store.snapshot()["scheduler"].get(str(aid), {})
        return bool(task.get("owns_pause") and task.get("release_requested_at") and task.get("test_success") is True
            and parse_iso_datetime(task.get("test_completed_at"))
            and task.get("release_control_generation") == self.store.control_generation(aid)
            and task.get("release_credential_fingerprint") == credential_fingerprint(row)
            and task.get("verification_method") == recovery_method(self.m.settings, aid)
            and task.get("verification_generation", 0) == int(metadata.get("recovery_method_generation", 0))
            and _signature(row) == task.get("signature") and self._eligible(row, now)
            and account_recovery_confirmed(row) and self._available(row, now, task))

    def _reconcile_release(self, aid, task, now):
        deadline = parse_iso_datetime(task.get("next_at"))
        if deadline and deadline > now:
            return
        with control_lock(self.m.db, aid):
            current = self.state(aid)
            if current.get("episode") != task.get("episode") or current.get("revision") != task.get("revision"):
                return
            row = self.m._read_account(aid)
            if self._release_confirmed(row, task, now):
                self._finish_release(row, task)
            else:
                self._save(aid, task, error_code="recovery_failed", next_at=(now + timedelta(seconds=30)).isoformat())

    def _finish_release(self, final, task):
        aid = int(final["id"])
        completed = self.m.clock().isoformat()
        finished = {**task, "revision": int(task.get("revision") or 0) + 1, "stage": "recovered", "owns_pause": False, "recovered_at": completed, "error_code": "", "next_at": None, "outcome": "restored", "completed_at": completed, "result_reason": ""}
        key = f"reset-credit:{aid}:{task['episode']}"
        history = {"account_id": aid, "account_name": str(final.get("name") or ""), "model_id": task["model_id"],
                   "legacy": False, "kind": "quota_recovery", "test_completed_at": task["test_completed_at"], "recovered_at": completed}
        if task.get("consumed") is True and task.get("reset_completed_at"):
            history.update(kind="reset_credit", reset_credit={"consumed": True, "completed_at": task["reset_completed_at"],
                "verification_method": task.get("verification_method") or "connection"})
        event = {**history, "status": "recovered", "stage": "recovery", "checked_at": completed,
                 "test_success": True, "window_labels": ["7d"], "fingerprint": task["episode"], "dedupe_key": key,
                 "plan_type": oauth_quota_summary_from_result(final, self._saved_quota(final, self.m.clock())).get("plan_type")}
        def finish(data):
            current = data["scheduler"].get(str(aid), {}).get("auto_reset_credit") or {}
            generation = int((data["scheduler"].get(str(aid), {}).get("manual_control") or {}).get("generation") or 0)
            if (current.get("episode") != task["episode"] or current.get("stage") != "releasing"
                    or current.get("revision") != task.get("revision") or generation != task.get("release_control_generation")):
                raise ValueError("自动恢复状态已变化")
            data["scheduler"][str(aid)]["auto_reset_credit"] = finished
            if key not in data["recovery_history"]:
                sequence = max([int(v.get("id") or 0) for v in data["recovery_history"].values()] + [0]) + 1
                data["recovery_history"][key] = {**history, "id": sequence, "dedupe_key": key}
                if task.get('policy_version'):
                    data['scheduler'][str(aid)].setdefault('credit_result_notifications', {}).setdefault(task['episode'],
                        self._notice(aid, finished, parse_iso_datetime(completed), final))
                else:
                    data["pending_events"][key] = event
        self.store.transaction(finish)
        self._audit(aid, "recovered", consumed=bool(task.get("consumed")))

    def manual(self, row: dict[str, Any], action: str, token: str, *, request_runner: Callable | None = None) -> dict[str, Any]:
        """Called under the existing account + monitor locks, after confirmation."""
        from .oauth_monitor import account_recovery_confirmed
        aid, now = int(row["id"]), self.m.clock()
        with self.store.disk_lock(f"credit-operation-{aid}", blocking=False):
            task = self.state(aid)
            if action == "reset" and task.get("attempt_at") and task.get("stage") != "recovered":
                return {"success": False, "error_code": "result_uncertain"}
            if action == "reset" or not task:
                next_task = {"episode": uuid.uuid4().hex, "stage": "manual" if action == "reset" else "closed",
                             "source": "manual"}
                def replace(data):
                    data["scheduler"].setdefault(str(aid), {})["auto_reset_credit"] = next_task
                self.store.transaction(replace)
                task = next_task
            result = self._request(row, action, task, now, source="manual", token=token, request_runner=request_runner)
            task = self.state(aid)
            live = self.m._read_account(aid)
            if (live and not task.get("owns_pause") and task.get("attempt_at") and self._eligible(live, self.m.clock())
                    and account_recovery_confirmed(live) and self._available(live, self.m.clock(), task)):
                self._save(aid, task, stage="recovered", recovered_at=self.m.clock().isoformat(), next_at=None, error_code="")
            return result
