from __future__ import annotations

from collections.abc import Callable
from datetime import datetime, timezone
from typing import Any

from . import account_ops
from .audit import write_audit
from .guard_classifier import classify_guard_event
from .guard_policy import GuardAction, GuardPolicy, GuardSignal, apply_signal
from .guard_store import GuardStore
from .sql import GUARD_ERROR_EVENTS_SQL, GUARD_ERROR_EVENTS_SQL_COMPAT_NO_LOAD_FACTOR, GUARD_SUCCESS_EVENTS_SQL


def is_oauth_account(row: dict[str, Any]) -> bool:
    account_type = str(row.get("account_type") or row.get("type") or "").strip().lower()
    return account_type == "oauth"


class GuardEngine:
    def __init__(
        self,
        db: Any,
        store: GuardStore,
        audit_path: str,
        policy: GuardPolicy,
        batch_size: int = 100,
        load_factor_supported: bool = False,
        endless_recovery_scheduler: Callable[[GuardAction, dict[str, Any]], dict[str, Any] | None] | None = None,
    ) -> None:
        self.db = db
        self.store = store
        self.audit_path = audit_path
        self.policy = policy
        self.batch_size = batch_size
        self.load_factor_supported = load_factor_supported
        self.endless_recovery_scheduler = endless_recovery_scheduler

    def run_once(self, actor: str = "auto_guard") -> list[dict[str, Any]]:
        actions: list[dict[str, Any]] = []
        rows = self.db.fetch_all(
            GUARD_ERROR_EVENTS_SQL if self.load_factor_supported else GUARD_ERROR_EVENTS_SQL_COMPAT_NO_LOAD_FACTOR,
            {"cursor_id": self.store.error_cursor(), "limit": self.batch_size},
        )
        max_cursor = self.store.error_cursor()
        for row in rows:
            max_cursor = max(max_cursor, int(row.get("error_log_id") or 0))
            account_id = row.get("account_id")
            if not account_id:
                continue
            if is_oauth_account(row):
                continue
            category = classify_guard_event(row)
            signal = GuardSignal(
                account_id=int(account_id),
                category=category,
                event_key=f"error:{int(row.get('error_log_id') or 0)}:{int(row.get('attempt_no') or 1)}",
                created_at=row.get("created_at") or datetime.now(timezone.utc),
                event_id=int(row.get("error_log_id") or 0),
                message=str(row.get("message") or ""),
            )
            circuit = self.store.circuit(int(account_id))
            action, circuit = apply_signal(self.policy, circuit, signal, datetime.now(timezone.utc))
            self.store.save_circuit(circuit)
            applied = self._apply_action(action, actor, row)
            if applied:
                actions.append(applied)
        self.store.set_error_cursor(max_cursor)
        self.record_successes()
        if actions:
            write_audit(self.audit_path, "guard_auto_actions", {"actor": actor, "actions": actions})
        return actions

    def _apply_action(self, action: GuardAction, actor: str, source_row: dict[str, Any]) -> dict[str, Any] | None:
        if action.kind == "pause":
            updated = account_ops.guard_pause_account(self.db, action.account_id, action.reason)
            if not updated:
                return None
            result = {
                "account_id": action.account_id,
                "name": updated.get("name") or source_row.get("account_name"),
                "action": "pause",
                "reason": action.reason,
                "event_id": action.event_id,
                "updated": updated,
                "actor": actor,
            }
            if int(action.account_id) in set(self.policy.endless_account_ids):
                result["endless_recovery_plan"] = self._schedule_endless_recovery(action, updated)
            write_audit(self.audit_path, "guard_auto_pause_account", result)
            return result

        if action.kind == "cooldown":
            if action.load_factor and self.load_factor_supported:
                account_ops.guard_update_account_routing(
                    self.db,
                    self.audit_path,
                    action.account_id,
                    actor,
                    int(source_row.get("account_priority") or source_row.get("priority") or 50),
                    int(action.load_factor),
                    True,
                    f"{action.reason}; automatic load-factor soft landing",
                )
            updated = account_ops.guard_cooldown_account(
                self.db,
                action.account_id,
                int(action.minutes or 15),
                action.reason,
            )
            if not updated:
                return None
            result = {
                "account_id": action.account_id,
                "name": updated.get("name") or source_row.get("account_name"),
                "action": "cooldown",
                "minutes": action.minutes,
                "load_factor": action.load_factor if self.load_factor_supported else None,
                "reason": action.reason,
                "event_id": action.event_id,
                "updated": updated,
                "actor": actor,
            }
            write_audit(self.audit_path, "guard_auto_cooldown_account", result)
            return result
        return None

    def _schedule_endless_recovery(self, action: GuardAction, updated: dict[str, Any]) -> dict[str, Any]:
        if self.endless_recovery_scheduler is None:
            return {"scheduled": False, "error": "endless recovery scheduler is not configured"}
        try:
            result = self.endless_recovery_scheduler(action, updated)
        except Exception as exc:
            return {"scheduled": False, "error": str(exc)}
        if isinstance(result, dict):
            return {"scheduled": bool(result.get("success", True)), **result}
        return {"scheduled": True}

    def record_successes(self) -> None:
        rows = self.db.fetch_all(
            GUARD_SUCCESS_EVENTS_SQL,
            {
                "cursor_created_at": self.store.success_cursor(),
                "limit": self.batch_size,
            },
        )
        latest = self.store.success_cursor()
        for row in rows:
            account_id = row.get("account_id")
            created_at = row.get("success_created_at")
            if not account_id or not created_at:
                continue
            latest = str(created_at)
            if is_oauth_account(row):
                continue
            signal = GuardSignal(
                account_id=int(account_id),
                category="success",
                event_key=str(row.get("success_event_key") or f"success:{account_id}:{created_at}"),
                created_at=created_at,
                message=f"{row.get('success_count') or 0} successful requests",
            )
            circuit = self.store.circuit(int(account_id))
            _, circuit = apply_signal(self.policy, circuit, signal, datetime.now(timezone.utc))
            self.store.save_circuit(circuit)
        self.store.set_success_cursor(latest)

    def record_recovery_success(self, account_id: int, result_id: int, message: str = "scheduled test recovered") -> bool:
        signal = GuardSignal(
            account_id=int(account_id),
            category="success",
            event_key=f"recovery:{int(result_id)}",
            created_at=datetime.now(timezone.utc),
            event_id=int(result_id),
            message=message,
        )
        circuit = self.store.circuit(int(account_id))
        if signal.event_key in set(circuit.processed_event_keys):
            return False
        account_ops.resume_account(
            self.db,
            self.audit_path,
            int(account_id),
            "auto_guard_recovery",
            message,
        )
        circuit.state = "half_open"
        _, circuit = apply_signal(self.policy, circuit, signal, datetime.now(timezone.utc))
        self.store.save_circuit(circuit)
        return True
