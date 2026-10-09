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
from typing import Any, Callable

from .audit import write_audit
from .desktop_usage import reset_credits
from .key_fallback import execute_sub2api_set_schedulable
from .account_locks import AccountLease, control_lock
from .oauth_queries import AUTH_ERRORS, automatic_eligible, credential_fingerprint, fresh_quota, quota_complete
from .quota_snapshot import latest_openai_result
from .usage_query import (oauth_quota_from_usage_data, oauth_quota_summary_from_result,
                          oauth_windows_by_key, parse_iso_datetime, percent_or_none,
                          required_oauth_window_keys)
from .error_evidence import is_local_throttle, local_throttle_sql
from .reset_credit_observation import (create_observation, digest, limit_fingerprint,
                                       normalized_credits, receipt_diagnostics, validate_observation, consumption_result)

STAGES = {"waiting", "pausing", "resetting", "uncertain", "testing", "retry", "confirming",
          "releasing", "recovered", "manual", "blocked", "closed"}
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
          "model_verification_pending": "等待模型验证", "model_verifier_unavailable": "模型验证服务尚未就绪"}
CREDIT_EVIDENCE_ERRORS = {"missing": "缺少可信卡观测", "content_changed": "卡快照内容已变化",
    "credentials_changed": "凭据已变化", "limit_changed": "限流事件已变化", "window_changed": "额度窗口已变化",
    "outside_window": "卡观测不在当前额度窗口内", "quota_missing": "缺少有效额度时间", "new_failure": "出现更新的查询失败证据"}


def validate_state(value: Any) -> None:
    if not isinstance(value, dict) or value.get("stage") not in STAGES or not value.get("episode"):
        raise ValueError("自动用卡状态无效")
    for key in ("attempt_at", "reset_at", "next_at", "test_completed_at", "recovered_at", "evidence_at",
                "reset_completed_at", "release_requested_at", "manual_at"):
        if value.get(key) and parse_iso_datetime(value[key]) is None:
            raise ValueError("自动用卡时间无效")
    if value.get("owns_pause") not in (True, False, None):
        raise ValueError("自动用卡暂停归属无效")


def project_state(value: dict[str, Any] | None) -> dict[str, Any] | None:
    if not value:
        return None
    if (value.get('stage') == 'manual' and value.get('attempt_at') and not value.get('reset_completed_at')
            and value.get('consumed') is not True and value.get('consumption_outcome') != 'not_consumed'):
        value = {**value, 'stage': 'uncertain', 'error_code': 'result_uncertain', 'next_at': None}
    elif value.get('stage') in {'manual', 'recovered', 'closed'}:
        return None
    code = str(value.get("error_code") or "")
    error = ERRORS.get(code, "操作未确认" if code else "")
    evidence_error = CREDIT_EVIDENCE_ERRORS.get(value.get("credit_evidence_error"))
    if value.get("stage") == "waiting" and evidence_error:
        error = "；".join(part for part in (error, evidence_error) if part)
    return {"stage": value["stage"], "label": {"conflict": "自动用卡冲突", "auth_paused": "等待凭据更新", "no_credit": "等待重置卡"}.get(code, STATE_LABELS[value["stage"]]),
            "error": error,
            "next_at": value.get("next_at"), "attempt_at": value.get("attempt_at"),
            "evidence_source": value.get("evidence_source"), "evidence_at": value.get("evidence_at"),
            "test_completed_at": value.get("test_completed_at"), "recovered_at": value.get("recovered_at")}


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
        ORDER BY e.created_at DESC,e.id DESC LIMIT 1
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


class AutoResetController:
    def __init__(self, monitor: Any, *, request_runner: Callable = execute_credit_request,
                 schedule_runner: Callable = execute_sub2api_set_schedulable,
                 evidence_reader: Callable = upstream_evidence) -> None:
        self.m = monitor
        self.store = monitor.store
        self.request_runner, self.schedule_runner, self.evidence_reader = request_runner, schedule_runner, evidence_reader
        self._last_scan: datetime | None = None

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
            meta["auto_reset_credit"] = updated
        self.store.transaction(save)
        return updated

    def _audit(self, account_id: int, action: str, **fields: Any) -> None:
        write_audit(self.m.settings.audit_path, "oauth_auto_reset_credit", {"account_id": account_id, "action": action, **fields})

    def _begin_episode(self, aid: int, old: dict[str, Any], evidence: dict[str, Any]) -> dict[str, Any]:
        task = {"episode": uuid.uuid4().hex, "stage": "waiting", **evidence}
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

    def _saved_quota(self, row, now):
        saved = self.store.snapshot()["oauth_results"].get(str(row["id"]))
        return latest_openai_result(row, saved, now)

    def _credit_known(self, row, evidence, now):
        return not self._credit_reason(row, evidence, now)

    def _credit_reason(self, row, evidence, now):
        """Reuse a verified observation, never date an old native cache as now."""
        meta = self.store.snapshot()["scheduler"].get(str(row["id"]), {})
        observation = meta.get("reset_credit_observation")
        raw = (row.get("extra") or {}).get("codex_reset_credit_snapshot") or {}
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
            if source == "automatic":
                if (live.get("extra") or {}).get("auto_reset_credit_enabled") is not False and (live.get("extra") or {}).get("auto_reset_credit_enabled") is not None:
                    return {"success": False, "error_code": "conflict"}
                if action == "reset":
                    # Re-read all evidence after budget admission, immediately
                    # before pausing and issuing the one consumption request.
                    depletion = self._depletion(live, now)
                    credits = reset_credits(live.get("extra") or {}, now)
                    if (not depletion or depletion["reset_at"] != task.get("reset_at")
                            or depletion["evidence_fingerprint"] != task.get("evidence_fingerprint")
                            or not self._credit_known(live, depletion, self.m.clock())
                            or (credits["available"] or 0) <= 0):
                        return {"success": False, "error_code": "reset_state_changed"}
                    with control_lock(self.m.db, aid):
                        active_task = self._save(aid, task, stage="pausing", signature=_signature(live), next_at=None,
                                                control_generation=self.store.control_generation(aid))
                        paused = self.schedule_runner(aid, False, **connection)
                        after = self.m._read_account(aid)
                        if (not paused.get("success") or not after or after.get("schedulable") is not False
                                or _signature(after) != active_task["signature"]):
                            self._save(aid, active_task, stage="blocked", error_code="pause_uncertain")
                            return {"success": False, "error_code": "pause_uncertain"}
                        active_task = self._save(aid, active_task, stage="resetting", owns_pause=True,
                                                owned_version=str(after.get("updated_at")), attempt_at=self.m.clock().isoformat())
                    # A second live read protects changes made during the pause.
                    final = self.m._read_account(aid)
                    final_depletion = self._depletion(final, self.m.clock(), held=True) if final else None
                    if (not self._owned(final, active_task) or not final_depletion
                            or final_depletion["reset_at"] != task.get("reset_at")
                            or final_depletion["evidence_fingerprint"] != task.get("evidence_fingerprint")
                            or not self._credit_known(final, final_depletion, self.m.clock())
                            or (reset_credits(final.get("extra") or {}, self.m.clock())["available"] or 0) <= 0):
                        self._save(aid, active_task, stage="blocked", error_code="ownership_changed")
                        return {"success": False, "error_code": "ownership_changed"}
                elif task.get("owns_pause") and not self._owned(live, task):
                    return {"success": False, "error_code": "ownership_changed"}
                elif not task.get("owns_pause") and not self._depletion(live, now):
                    return {"success": False, "error_code": "reset_state_changed"}
            elif action == "reset":
                active_task = self._save(aid, task, stage="uncertain", attempt_at=self.m.clock().isoformat())
            if self.state(aid).get("revision") != active_task.get("revision"):
                return {"success": False, "error_code": "manual_intervention"}
            requested_at = self.m.clock()
            result = (request_runner or self.request_runner)(action, aid, **connection)
            completed = self.m.clock()
            diagnostics = result.get("diagnostics") or receipt_diagnostics(None, result.get("data"))
            # Persist confirmed consumption before the coordinator's quota write;
            # a failed latter write cannot turn success into a repeatable request.
            if action == "reset":
                outcome = result.get("consumption_outcome") or ("consumed" if result.get("consumed") else "unknown")
                receipt_code = "" if result.get("consumed") else str(result.get("error_code") or "result_uncertain")
                latest = self.state(aid)
                if latest.get("episode") == active_task.get("episode") and latest.get("stage") == "manual":
                    # A scheduling intervention revokes recovery authority, not
                    # the irreversible consumption receipt from an in-flight call.
                    self._save(aid, latest, consumed=bool(result.get("consumed")),
                               reset_completed_at=completed.isoformat() if result.get("consumed") else None,
                               error_code=receipt_code, consumption_outcome=outcome, receipt=diagnostics)
                    return result
                stage = ("manual" if source != "automatic" and outcome != "unknown" else
                         "testing" if result.get("consumed") else "confirming" if outcome == "not_consumed" else "uncertain")
                active_task = self._save(aid, active_task, stage=stage,
                    consumed=bool(result.get("consumed")), reset_completed_at=completed.isoformat() if result.get("consumed") else None,
                    consumption_outcome=outcome, error_code=receipt_code, receipt=diagnostics)
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
                                             operation=operation, validate=valid, now=now)
        self._audit(aid, action, source=source, success=bool(result.get("success")),
                    error_code=result.get("error_code", ""), next_at=result.get("next_query_at"),
                    diagnostics=result.get("diagnostics", {}))
        return result

    def run(self, rows: list[dict[str, Any]], now: datetime) -> None:
        if self._last_scan and (now - self._last_scan).total_seconds() < 30:
            return
        self._last_scan = now
        enabled = bool(getattr(self.m.settings, "oauth_auto_reset_credit_enabled", False))
        state = self.store.snapshot()
        existing = state["scheduler"]
        if not enabled and not any((v.get("auto_reset_credit") or {}).get("owns_pause") for v in existing.values()):
            return
        try:
            with self.store.disk_lock("credit-operation", blocking=False):
                for item in rows:
                    aid = int(item["id"])
                    lease = AccountLease(self.m.db, item)
                    if not lease.acquire():
                        continue
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

    def _step(self, aid: int, now: datetime, enabled: bool) -> None:
        from .oauth_monitor import account_recovery_confirmed
        row, task = self.m._read_account(aid), self.state(aid)
        if not row:
            return
        from .recovery_policy import recovery_method
        if not recovery_method(self.m.settings, aid) or not getattr(self.m.settings, 'oauth_recovery_monitor_enabled', True):
            return
        if (row.get("extra") or {}).get("auto_reset_credit_enabled"):
            if task or enabled:
                task = task or {"episode": uuid.uuid4().hex, "stage": "waiting"}
                if task.get("error_code") != "conflict":
                    self._save(aid, task, error_code="conflict")
                    self._audit(aid, "conflict")
            return
        if task.get("error_code") == "conflict":
            task = self._save(aid, task, error_code="")
        if task.get("stage") in {"manual", "blocked"}:
            if self._eligible(row, now) and self._available(row, now, task):
                if account_recovery_confirmed(row):
                    self._save(aid, task, stage="recovered", recovered_at=now.isoformat(), error_code="", next_at=None)
                return
            # Manual control revokes this task, not every future depletion. An
            # unissued task can be replaced only after an explicit re-enable and
            # a new upstream event. Any issued consumption must recover first.
            control = self.store.snapshot()["scheduler"].get(str(aid), {}).get("manual_control") or {}
            changed = parse_iso_datetime(control.get("at"))
            terminal = parse_iso_datetime(task.get("manual_at"))
            evidence = self._depletion(row, now) if enabled and not task.get("attempt_at") else None
            boundary = max((t for t in (changed, terminal) if t), default=None)
            if (not evidence or control.get("enabled") is not True or not changed or not boundary
                    or parse_iso_datetime(evidence["evidence_at"]) <= boundary
                    or evidence["evidence_fingerprint"] == task.get("evidence_fingerprint")):
                return
            task = self._begin_episode(aid, task, evidence)
        if task.get("auth_fingerprint"):
            if task["auth_fingerprint"] == credential_fingerprint(row):
                return
            # Credential changes release the auth pause only when the account
            # still belongs to this recovery hold and its other config matches.
            if task.get("owns_pause") and not self._owned(row, task, same_version=False):
                return
            task = self._save(aid, task, auth_fingerprint=None, auth_recheck=True,
                              owned_version=str(row.get("updated_at")), next_at=None, error_code="")
        if task.get("stage") in {"pausing", "resetting", "testing", "releasing"}:
            if task["stage"] == "pausing":
                self._save(aid, task, stage="blocked", error_code="pause_uncertain")
                return
            if task["stage"] == "testing":
                task = self._save(aid, task, stage="retry")
            if task["stage"] == "resetting":
                task = self._save(aid, task, stage="uncertain", error_code="result_uncertain")
            if task["stage"] == "releasing":
                self._reconcile_release(aid, task, now)
                return
        if task.get("owns_pause"):
            if not self._owned(row, task) or not self._processing_eligible(row, task, now):
                self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
                return
            deadline = parse_iso_datetime(task.get("next_at"))
            if deadline and deadline > now:
                return
            if task.get("stage") in {"testing", "retry"}:
                self._test(row, task, now)
                return
            if task.get("stage") in {"uncertain", "confirming"}:
                if not self._available(row, now, task):
                    result = self._request(row, "query", task, now)
                    task = self.state(aid)
                    if not result.get("success"):
                        self._defer(aid, task, result, now)
                        return
                    row = self.m._read_account(aid)
                    task = self.state(aid)
                if row and self._available(row, now, task):
                    # The observable recovery is sufficient for a safe test;
                    # an uncertain consumption is never reclassified as certain.
                    self._test(row, task, now)
                else:
                    self._defer(aid, task, {"error_code": "quota_unavailable"}, now)
                return
            return
        if not enabled:
            return
        if task.get("attempt_at") and task.get("stage") != "recovered":
            # Manual/uncertain attempts can close only after observed availability.
            if self._eligible(row, now) and account_recovery_confirmed(row) and self._available(row, now, task):
                self._save(aid, task, stage="recovered", recovered_at=now.isoformat(), error_code="", next_at=None)
            return
        evidence = self._depletion(row, now)
        control = self.store.snapshot()["scheduler"].get(str(aid), {}).get("manual_control") or {}
        changed = parse_iso_datetime(control.get("at"))
        if evidence and changed and (parse_iso_datetime(evidence.get("evidence_at")) or changed) <= changed:
            return
        if not evidence:
            if task.get("stage") == "waiting" and not task.get("attempt_at") and (
                    self._available(row, now, task) or not row.get("rate_limited_at")):
                self._save(aid, task, stage="closed", recovered_at=now.isoformat(), error_code="", next_at=None)
            return
        if task.get("stage") in {"recovered", "closed"}:
            recovered = parse_iso_datetime(task.get("recovered_at"))
            if not recovered or parse_iso_datetime(evidence["evidence_at"]) <= recovered:
                return
            # Retain the previous episode until a genuinely new 429 after recovery.
            task = self._begin_episode(aid, task, evidence)
        elif not task:
            task = self._save(aid, {"episode": uuid.uuid4().hex, "stage": "waiting", **evidence})
        elif any(task.get(key) != evidence[key] for key in ("reset_at", "evidence_source", "evidence_fingerprint")):
            task = self._save(aid, task, **evidence, evidence_audited=False, next_at=None, error_code="")
        if not task.get("evidence_audited"):
            task = self._save(aid, task, evidence_audited=True)
            self._audit(aid, "evidence", source=task.get("evidence_source"),
                        evidence_fingerprint=task.get("evidence_fingerprint"))
        deadline = parse_iso_datetime(task.get("next_at"))
        if deadline and deadline > now:
            return
        connection = self._connection()
        if not connection["base_url"] or not connection["admin_token"]:
            return
        credits = reset_credits(row.get("extra") or {}, now)
        if task.get("error_code") == "no_credit" and (credits["available"] or 0) <= 0:
            return
        quota = self._saved_quota(row, now)
        quota_at = parse_iso_datetime((quota or {}).get("queried_at"))
        # Card evidence must describe this depletion window. A positive card
        # snapshot may be reused after the mandatory one-hour query cooldown.
        credit_reason = self._credit_reason(row, evidence, now)
        known = not credit_reason
        meta = self.store.snapshot()["scheduler"].get(str(aid), {})
        failed = parse_iso_datetime(meta.get("last_error_at"))
        invalidated = bool(failed and quota_at and failed >= quota_at and meta.get("last_error_code"))
        credit_reason = credit_reason or ("new_failure" if invalidated else "")
        if task.get("credit_evidence_error", "") != credit_reason:
            task = self._save(aid, task, credit_evidence_error=credit_reason)
            if credit_reason:
                self._audit(aid, "credit_evidence_invalidated", reason=credit_reason)
        if not known or invalidated or (credits["available"] or 0) <= 0:
            result = self._request(row, "query", task, now)
            if not result.get("success"):
                self._defer(aid, self.state(aid), result, now)
                return
            live = self.m._read_account(aid)
            no_credit = not live or (reset_credits(live.get("extra") or {}, now)["available"] or 0) <= 0
            self._defer(aid, self.state(aid), {"error_code": "no_credit" if no_credit else "query_cooldown"}, now)
            return
        if recovery_method(self.m.settings, aid) == 'model':
            verifier = getattr(self.m, 'model_verifier', None)
            if verifier is None:
                self._save(aid, task, error_code='model_verifier_unavailable')
                return
            result = verifier(aid, kind='credit', fingerprint=task['episode'], attempt=int(task.get('test_attempts') or 0) + 1, consume=True)
            self._save(aid, self.state(aid), next_at=result.get('next_at') or (now + timedelta(minutes=1)).isoformat(),
                       model_test_job_id=result.get('job_id'), error_code='model_verification_pending' if result.get('deferred') else result.get('error_code', ''))
            return
        result = self._request(row, "reset", task, now)
        task = self.state(aid)
        if result.get("consumed") and task.get("stage") == "testing" and not result.get("uncertain"):
            live = self.m._read_account(aid)
            if self._owned(live, task):
                self._test(live, task, self.m.clock())
        elif task.get("stage") not in {"blocked", "manual"}:
            self._defer(aid, task, result, now)

    def _defer(self, aid, task, result, now):
        code = str(result.get("error_code") or "incomplete_quota")
        changes: dict[str, Any] = {"error_code": code, "next_at": result.get("next_query_at") or (now + timedelta(hours=1)).isoformat()}
        if code in AUTH_ERRORS:
            row = self.m._read_account(aid)
            changes.update(auth_fingerprint=credential_fingerprint(row or {}), error_code="auth_paused", next_at=None)
        if code == "no_credit":
            changes.update(next_at=None)
        self._save(aid, task, **changes)

    def _test(self, row, task, now):
        aid = int(row["id"])
        if not self._owned(row, task) or not self._processing_eligible(row, task, now):
            self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
            return
        model = str(getattr(self.m.settings, "oauth_recovery_test_model_id", "gpt-5.6-luna"))
        attempts = int(task.get("test_attempts") or 0) + 1
        task = self._save(aid, task, stage="testing", test_attempts=attempts,
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
            test = self.m._timed_test(aid, model, control_generation=generation, **self._connection())
        else:
            return
        if (self.store.control_generation(aid) != generation or method != recovery_method(self.m.settings, aid)
                or selection != int(self.store.snapshot()['scheduler'].get(str(aid), {}).get('recovery_method_generation', 0))):
            return
        if test.get('deferred'):
            self._save(aid, task, stage='retry', test_attempts=attempts - 1,
                       next_at=test.get('next_at') or (now + timedelta(seconds=5)).isoformat(),
                       model_test_job_id=test.get('job_id'), error_code='model_verification_pending')
            return
        model = test.get('model_id') or model
        self.m._cycle_tests[aid] = test
        completed = self.m.clock()
        self._audit(aid, "test", success=bool(test.get("success")), error_code=str(test.get("error_code") or ""))
        if self.store.control_generation(aid) != generation:
            return
        live = self.m._read_account(aid)
        code = str(test.get("error_code") or "test_failed")
        auth_error = code in AUTH_ERRORS and not test.get("success")
        candidate = {**live, "status": "active"} if live and auth_error and live.get("status") == "error" else live
        if not self._owned(live, task, same_version=False) or not self._processing_eligible(candidate, task, completed):
            self._save(aid, task, stage="blocked", owns_pause=False, error_code="ownership_changed")
            return
        task = self._save(aid, task, owned_version=str(live.get("updated_at")), test_completed_at=completed.isoformat(),
                          test_success=bool(test.get("success")), model_id=model,
                          verification_method=method, verification_generation=selection)
        if not test.get("success"):
            self._save(aid, task, stage="retry", error_code="auth_paused" if code in AUTH_ERRORS else "test_failed",
                       auth_fingerprint=credential_fingerprint(live) if code in AUTH_ERRORS else None,
                       auth_recheck=False,
                       auth_error=hashlib.sha256(str(live.get("error_message") or "").encode()).hexdigest() if code in AUTH_ERRORS else None,
                       next_at=None if code in AUTH_ERRORS else (completed + timedelta(seconds=RETRY_SECONDS[min(attempts - 1, 3)])).isoformat())
            return
        if not self._available(live, completed, task):
            self._save(aid, task, stage="confirming", error_code="incomplete_quota", next_at=(completed + timedelta(hours=1)).isoformat())
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
        from .recovery_policy import recovery_method
        if (task.get('verification_method') and task['verification_method'] != recovery_method(self.m.settings, aid)
                or task.get('verification_generation', 0) != int(self.store.snapshot()['scheduler'].get(str(aid), {}).get('recovery_method_generation', 0))):
            return
        fresh = self.m._read_account(aid)
        if (not self._owned(fresh, task) or not self._processing_eligible(fresh, task, now)
                or not self._available(fresh, now, task)):
            self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
            return
        # Clearing an old block is allowed only after a successful test and fresh
        # available quota. Do not overwrite a newer rate-limit or manual pause.
        for key in ("rate_limited_at", "temp_unschedulable_until", "overload_until"):
            stamp = parse_iso_datetime(fresh.get(key))
            if stamp and stamp > (parse_iso_datetime(task.get("attempt_at")) or now):
                self._save(aid, task, stage="confirming", error_code="quota_unavailable", next_at=(now + timedelta(hours=1)).isoformat())
                return
        recovered = self.m.recovery_runner(aid, **self._connection())
        cleared = self.m._read_account(aid)
        if (not recovered.get("success") or not self._owned(cleared, task, same_version=False)
                or not account_recovery_confirmed({**(cleared or {}), "schedulable": True})):
            self._save(aid, task, stage="confirming", error_code="recovery_failed", next_at=(now + timedelta(minutes=30)).isoformat())
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
            self._save(aid, task, stage="confirming", error_code="incomplete_quota",
                       next_at=(self.m.clock() + timedelta(hours=1)).isoformat())
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
        finished = {**task, "stage": "recovered", "owns_pause": False, "recovered_at": completed, "error_code": "", "next_at": None}
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
                data["pending_events"][key] = event
        self.store.transaction(finish)
        self._audit(aid, "recovered", consumed=bool(task.get("consumed")))

    def manual(self, row: dict[str, Any], action: str, token: str, *, request_runner: Callable | None = None) -> dict[str, Any]:
        """Called under the existing account + monitor locks, after confirmation."""
        from .oauth_monitor import account_recovery_confirmed
        aid, now = int(row["id"]), self.m.clock()
        with self.store.disk_lock("credit-operation", blocking=False):
            task = self.state(aid)
            if action == "reset" and task.get("attempt_at") and task.get("stage") != "recovered":
                return {"success": False, "error_code": "result_uncertain"}
            if action == "reset" or not task:
                next_task = {"episode": uuid.uuid4().hex, "stage": "manual" if action == "reset" else "waiting"}
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
