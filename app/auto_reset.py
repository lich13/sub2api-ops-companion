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

STAGES = {"waiting", "pausing", "resetting", "uncertain", "testing", "retry", "confirming",
          "releasing", "recovered", "manual", "blocked", "closed"}
RETRY_SECONDS = (60, 300, 900, 1800)
STATE_LABELS = {"waiting": "等待用卡", "pausing": "暂停调度中", "resetting": "重置中",
                "uncertain": "重置待确认", "testing": "测活中", "retry": "等待重试测活",
                "confirming": "等待额度确认", "releasing": "恢复调度中",
                "recovered": "用卡恢复完成", "manual": "已转人工处理", "blocked": "自动用卡已暂停", "closed": "等待已结束"}
ERRORS = {"no_credit": "没有可用重置卡", "conflict": "Sub2API 自动用卡已开启，存在冲突",
          "ownership_changed": "账号已被其他操作修改，请人工确认调度状态",
          "pause_uncertain": "暂停结果待确认，未发送用卡请求", "auth_paused": "认证异常，等待凭据更新",
          "result_uncertain": "重置结果待确认，禁止重复用卡", "reset_state_changed": "账号或额度证据已变化",
          "query_state_unavailable": "状态无法保存，已暂停自动处理", "test_failed": "测活失败",
          "incomplete_quota": "额度证据不完整", "query_cooldown": "等待查询冷却",
          "query_budget": "等待查询预算", "query_backoff": "等待查询退避",
          "quota_unavailable": "必要额度窗口尚未恢复", "recovery_failed": "恢复调度未确认"}


def validate_state(value: Any) -> None:
    if not isinstance(value, dict) or value.get("stage") not in STAGES or not value.get("episode"):
        raise ValueError("自动用卡状态无效")
    for key in ("attempt_at", "reset_at", "next_at", "test_completed_at", "recovered_at", "evidence_at"):
        if value.get(key) and parse_iso_datetime(value[key]) is None:
            raise ValueError("自动用卡时间无效")
    if value.get("owns_pause") not in (True, False, None):
        raise ValueError("自动用卡暂停归属无效")


def project_state(value: dict[str, Any] | None) -> dict[str, Any] | None:
    if not value or value.get("stage") == "closed":
        return None
    code = str(value.get("error_code") or "")
    return {"stage": value["stage"], "label": {"conflict": "自动用卡冲突", "auth_paused": "等待凭据更新", "no_credit": "等待重置卡"}.get(code, STATE_LABELS[value["stage"]]),
            "error": ERRORS.get(code, "操作未确认" if code else ""),
            "next_at": value.get("next_at"), "attempt_at": value.get("attempt_at"),
            "test_completed_at": value.get("test_completed_at"), "recovered_at": value.get("recovered_at")}


def upstream_evidence(db: Any, row: dict[str, Any], now: datetime) -> dict[str, Any] | None:
    start = parse_iso_datetime(row.get("rate_limited_at"))
    if not start:
        return None
    # status_code alone can represent client-key throttling. Only an actual
    # upstream 429 associated with this still-active account block is evidence.
    return db.fetch_one("""
        SELECT e.id,e.created_at FROM ops_error_logs e
        WHERE e.account_id=%(id)s AND e.upstream_status_code=429
          AND e.created_at >= %(start)s AND e.created_at <= %(now)s
          AND NOT EXISTS (SELECT 1 FROM usage_logs u WHERE u.account_id=e.account_id AND u.created_at>e.created_at)
          AND NOT EXISTS (SELECT 1 FROM ops_error_logs x WHERE x.account_id=e.account_id
            AND x.created_at>e.created_at AND x.upstream_status_code IN (401,402))
        ORDER BY e.created_at DESC,e.id DESC LIMIT 1
    """, {"id": int(row["id"]), "start": start - timedelta(seconds=5), "now": now})


def _signature(row: dict[str, Any]) -> str:
    credentials = row.get("credentials") or {}
    stable = {key: row.get(key) for key in ("id", "name", "platform", "type", "parent_account_id",
              "account_priority", "priority", "concurrency", "expires_at", "auto_pause_on_expired")}
    stable["identity"] = {key: credentials.get(key) for key in ("plan_type", "chatgpt_account_id", "email")}
    # Preserve configuration ownership; exclude only runtime quota snapshots.
    stable["extra"] = {k: v for k, v in (row.get("extra") or {}).items()
                       if not k.startswith(("codex_5h_", "codex_7d_", "codex_usage_", "codex_reset_credit_", "codex_credits_"))}
    return hashlib.sha256(json.dumps(stable, sort_keys=True, default=str).encode()).hexdigest()


def quota_result(data: dict[str, Any], row: dict[str, Any], now: datetime) -> dict[str, Any]:
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
    observed = parse_iso_datetime(payload.get("fetched_at")) or now
    return {"account_id": int(row["id"]), "template_type": "oauth", "success": bool(normal),
            "source": "sub2api_admin_usage", "queried_at": observed.isoformat(),
            "oauth_quota": oauth_quota_from_usage_data(normal, row, now=observed)}


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def execute_credit_request(action: str, account_id: int, *, base_url: str, admin_token: str,
                           timeout_seconds: int = 90, urlopen: Callable | None = None) -> dict[str, Any]:
    path = "reset-quota" if action == "reset" else "quota/refresh"
    request = urllib.request.Request(f"{base_url.rstrip('/')}/api/v1/admin/openai/accounts/{account_id}/{path}",
        data=b"{}", method="POST", headers={"x-api-key": admin_token, "Accept": "application/json", "Content-Type": "application/json"})
    try:
        with (urlopen or urllib.request.build_opener(_NoRedirect()).open)(request, timeout=timeout_seconds) as response:
            body = json.loads(response.read(2_000_000))
        data = body.get("data")
        if body.get("code") != 0 or not isinstance(data, dict):
            return {"success": False, "uncertain": action == "reset", "error_code": "result_uncertain"}
        windows_reset = data.get("windows_reset")
        consumed = (action == "reset" and data.get("code") in {"success", "ok", 0}
                    and isinstance(windows_reset, int) and not isinstance(windows_reset, bool) and windows_reset > 0)
        return {"success": bool(consumed) if action == "reset" else True, "consumed": consumed,
                "uncertain": action == "reset" and not consumed,
                "error_code": "" if action != "reset" or consumed else "result_uncertain", "data": data}
    except urllib.error.HTTPError as exc:
        return {"success": False, "uncertain": action == "reset", "error_code": f"http_{exc.code}"}
    except Exception:
        return {"success": False, "uncertain": action == "reset", "error_code": "result_uncertain"}


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

    def cancel(self, account_id: int) -> None:
        task = self.state(account_id)
        if task and task.get("stage") != "recovered":
            self._save(account_id, task, stage="manual", owns_pause=False, next_at=None, error_code="")
            self._audit(account_id, "manual_intervention")

    def _saved_quota(self, row, now):
        saved = self.store.snapshot()["oauth_results"].get(str(row["id"]))
        return latest_openai_result(row, saved, now)

    def _eligible(self, row, now, *, held=False):
        return bool(row and not row.get("parent_account_id")
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
        return {"reset_at": reset.isoformat(), "window_minutes": duration,
                "evidence_at": stamp.isoformat(), "evidence_id": str(evidence["id"])}

    def _available(self, row, now, task):
        result = self._saved_quota(row, now)
        observed = parse_iso_datetime((result or {}).get("queried_at"))
        attempted = parse_iso_datetime(task.get("attempt_at"))
        if not fresh_quota(row, result, now) or not observed or (attempted and observed < attempted):
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
                            or (credits["available"] or 0) <= 0):
                        return {"success": False, "error_code": "reset_state_changed"}
                    with control_lock(self.m.db, aid):
                        active_task = self._save(aid, task, stage="pausing", signature=_signature(live), next_at=None)
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
            result = (request_runner or self.request_runner)(action, aid, **connection)
            completed = self.m.clock()
            if isinstance(result.get("data"), dict):
                result["quota_result"] = quota_result(result["data"], live, completed)
                if action == "query" and not quota_complete(live, result["quota_result"]):
                    result.update(success=False, error_code="incomplete_quota")
            # Persist confirmed consumption before the coordinator's quota write;
            # a failed latter write cannot turn success into a repeatable request.
            if action == "reset":
                latest = self.state(aid)
                if latest.get("episode") == active_task.get("episode") and latest.get("stage") == "manual":
                    # A scheduling intervention revokes recovery authority, not
                    # the irreversible consumption receipt from an in-flight call.
                    self._save(aid, latest, consumed=bool(result.get("consumed")),
                               reset_completed_at=completed.isoformat() if result.get("consumed") else None,
                               error_code="" if result.get("consumed") else "result_uncertain")
                    return result
                active_task = self._save(aid, active_task, stage="testing" if result.get("consumed") and source == "automatic" else "uncertain",
                    consumed=bool(result.get("consumed")), reset_completed_at=completed.isoformat() if result.get("consumed") else None,
                    error_code="" if result.get("consumed") else "result_uncertain")
            after = self.m._read_account(aid)
            if active_task.get("owns_pause") and self._owned(after, active_task, same_version=False):
                self._save(aid, active_task, owned_version=str(after.get("updated_at")))
            return result
        result = self.m.queries.external_read(row, source=source, reason="reset_credit_consume" if action == "reset" else "reset_credit_check",
                                             operation=operation, validate=valid, now=now)
        self._audit(aid, action, source=source, success=bool(result.get("success")),
                    error_code=result.get("error_code", ""), next_at=result.get("next_query_at"))
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
        row, task = self.m._read_account(aid), self.state(aid)
        if not row:
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
        if task.get("stage") == "blocked":
            return
        if task.get("stage") == "manual":
            if self._eligible(row, now) and self._available(row, now, task):
                self._save(aid, task, stage="recovered", recovered_at=now.isoformat(), error_code="", next_at=None)
            return
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
                # A crash during release must not repeat a scheduling mutation.
                self._save(aid, task, stage="blocked", error_code="recovery_failed")
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
            if self._available(row, now, task):
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
            old = task
            task = {"episode": uuid.uuid4().hex, "stage": "waiting", **evidence}
            def replace(data):
                current = data["scheduler"].setdefault(str(aid), {}).get("auto_reset_credit") or {}
                if current.get("episode") != old["episode"]:
                    raise ValueError("自动用卡任务已变化")
                data["scheduler"][str(aid)]["auto_reset_credit"] = task
            self.store.transaction(replace)
        elif not task:
            task = self._save(aid, {"episode": uuid.uuid4().hex, "stage": "waiting", **evidence})
        elif task.get("reset_at") != evidence["reset_at"]:
            task = self._save(aid, task, **evidence, next_at=None, error_code="")
        deadline = parse_iso_datetime(task.get("next_at"))
        if deadline and deadline > now:
            return
        connection = self._connection()
        if not connection["base_url"] or not connection["admin_token"]:
            return
        credits = reset_credits(row.get("extra") or {}, now)
        if task.get("error_code") == "no_credit" and (credits["available"] or 0) <= 0:
            return
        observed = credits["observed_at"]
        quota = self._saved_quota(row, now)
        quota_at = parse_iso_datetime((quota or {}).get("queried_at"))
        # Card evidence must describe this depletion window. A positive card
        # snapshot may be reused after the mandatory one-hour query cooldown.
        known = bool(observed and quota_at and observed <= now
                     and observed >= parse_iso_datetime(evidence["reset_at"]) - timedelta(minutes=evidence["window_minutes"])
                     and observed < parse_iso_datetime(evidence["reset_at"]))
        meta = self.store.snapshot()["scheduler"].get(str(aid), {})
        failed = parse_iso_datetime(meta.get("last_error_at"))
        invalidated = bool(failed and quota_at and failed >= quota_at and meta.get("last_error_code"))
        if not known or invalidated or (credits["available"] or 0) <= 0:
            result = self._request(row, "query", task, now)
            if not result.get("success"):
                self._defer(aid, self.state(aid), result, now)
                return
            live = self.m._read_account(aid)
            no_credit = not live or (reset_credits(live.get("extra") or {}, now)["available"] or 0) <= 0
            self._defer(aid, self.state(aid), {"error_code": "no_credit" if no_credit else "query_cooldown"}, now)
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
        test = self.m._timed_test(aid, model, control_generation=generation, **self._connection())
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
                          test_success=bool(test.get("success")), model_id=model)
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
        task = self._save(aid, task, stage="releasing", owned_version=str(cleared.get("updated_at")), next_at=None)
        verified = self.m._read_account(aid)
        if not self._owned(verified, task) or not self._processing_eligible(verified, task, self.m.clock()):
            self._save(aid, task, stage="blocked", error_code="ownership_changed", owns_pause=False)
            return
        if not self._available(verified, self.m.clock(), task):
            self._save(aid, task, stage="confirming", error_code="incomplete_quota",
                       next_at=(self.m.clock() + timedelta(hours=1)).isoformat())
            return
        result = self.schedule_runner(aid, True, **self._connection())
        final = self.m._read_account(aid)
        if not result.get("success") or not final or _signature(final) != task["signature"] or not account_recovery_confirmed(final):
            self._save(aid, task, stage="blocked", error_code="recovery_failed", owns_pause=False)
            return
        completed = self.m.clock().isoformat()
        finished = {**task, "stage": "recovered", "owns_pause": False, "recovered_at": completed, "error_code": "", "next_at": None}
        key = f"reset-credit:{aid}:{task['episode']}"
        history = {"account_id": aid, "account_name": str(final.get("name") or ""), "model_id": task["model_id"],
                   "legacy": False, "test_completed_at": task["test_completed_at"], "recovered_at": completed}
        event = {**history, "status": "recovered", "stage": "recovery", "checked_at": completed,
                 "test_success": True, "window_labels": ["7d"], "fingerprint": task["episode"], "dedupe_key": key,
                 "plan_type": oauth_quota_summary_from_result(final, self._saved_quota(final, self.m.clock())).get("plan_type")}
        def finish(data):
            current = data["scheduler"].get(str(aid), {}).get("auto_reset_credit") or {}
            if current.get("episode") != task["episode"] or current.get("stage") != "releasing":
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
            if live and not task.get("owns_pause") and task.get("attempt_at") and self._available(live, self.m.clock(), task):
                self._save(aid, task, stage="recovered", recovered_at=self.m.clock().isoformat(), next_at=None, error_code="")
            return result
