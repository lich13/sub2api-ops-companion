from __future__ import annotations

import json
from dataclasses import asdict, fields
from pathlib import Path
from typing import Any

from .guard_policy import GuardCircuit
from .guard_policy import normalize_account_ids


class GuardStore:
    def __init__(self, path: str) -> None:
        self.path = Path(path)
        self._data = self._read()

    def _read(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return {"cursors": {}, "circuits": {}, "policy": {}}
        if not isinstance(data, dict):
            return {"cursors": {}, "circuits": {}, "policy": {}}
        data.setdefault("cursors", {})
        data.setdefault("circuits", {})
        data.setdefault("policy", {})
        data.setdefault("endless_recovery_plans", {})
        return data

    def _write(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(self.path.suffix + ".tmp")
        tmp.write_text(json.dumps(self._data, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")
        tmp.replace(self.path)

    def error_cursor(self) -> int:
        return max(0, int((self._data.get("cursors") or {}).get("error_log_id") or 0))

    def set_error_cursor(self, value: int) -> None:
        self._data.setdefault("cursors", {})["error_log_id"] = max(0, int(value or 0))
        self._write()

    def success_cursor(self) -> str:
        return str((self._data.get("cursors") or {}).get("success_created_at") or "")

    def set_success_cursor(self, value: str) -> None:
        self._data.setdefault("cursors", {})["success_created_at"] = str(value or "")
        self._write()

    def recovery_cursor(self) -> int:
        return max(0, int((self._data.get("cursors") or {}).get("scheduled_recovery_result_id") or 0))

    def set_recovery_cursor(self, value: int) -> None:
        self._data.setdefault("cursors", {})["scheduled_recovery_result_id"] = max(0, int(value or 0))
        self._write()

    def circuit(self, account_id: int) -> GuardCircuit:
        raw = (self._data.get("circuits") or {}).get(str(int(account_id))) or {}
        if not isinstance(raw, dict):
            raw = {}
        allowed = {field.name for field in fields(GuardCircuit)}
        payload = {key: value for key, value in raw.items() if key in allowed and key != "account_id"}
        return GuardCircuit(account_id=int(account_id), **payload)

    def save_circuit(self, circuit: GuardCircuit) -> None:
        self._data.setdefault("circuits", {})[str(int(circuit.account_id))] = asdict(circuit)
        self._write()

    def policy_config(self) -> dict[str, Any]:
        raw = self._data.get("policy") or {}
        return raw if isinstance(raw, dict) else {}

    def save_policy(self, policy: dict[str, Any]) -> None:
        self._data["policy"] = dict(policy)
        self._write()

    def endless_recovery_plan(self, account_id: int) -> dict[str, Any]:
        raw = (self._data.get("endless_recovery_plans") or {}).get(str(int(account_id))) or {}
        return raw if isinstance(raw, dict) else {}

    def save_endless_recovery_plan(self, account_id: int, plan: dict[str, Any]) -> None:
        self._data.setdefault("endless_recovery_plans", {})[str(int(account_id))] = dict(plan)
        self._write()

    def clear_endless_recovery_plan(self, account_id: int) -> dict[str, Any]:
        plans = self._data.setdefault("endless_recovery_plans", {})
        existing = plans.pop(str(int(account_id)), None)
        if existing is not None:
            self._write()
        return existing if isinstance(existing, dict) else {}

    def add_whitelist_account(self, account_id: int) -> tuple[dict[str, Any], bool]:
        policy = dict(self.policy_config())
        current = set(normalize_account_ids(policy.get("whitelist_account_ids")))
        endless = set(normalize_account_ids(policy.get("endless_account_ids")))
        before = set(current)
        current.add(int(account_id))
        endless.discard(int(account_id))
        normalized = sorted(current)
        normalized_endless = sorted(endless)
        needs_write = policy.get("whitelist_account_ids") != normalized or policy.get("endless_account_ids") != normalized_endless
        policy["whitelist_account_ids"] = normalized
        policy["endless_account_ids"] = normalized_endless
        changed = current != before or int(account_id) in set(normalize_account_ids(self.policy_config().get("endless_account_ids")))
        if changed or needs_write:
            self.save_policy(policy)
        return policy, changed

    def remove_whitelist_account(self, account_id: int) -> tuple[dict[str, Any], bool]:
        policy = dict(self.policy_config())
        current = set(normalize_account_ids(policy.get("whitelist_account_ids")))
        before = set(current)
        current.discard(int(account_id))
        normalized = sorted(current)
        needs_write = policy.get("whitelist_account_ids") != normalized
        policy["whitelist_account_ids"] = normalized
        changed = current != before
        if changed or needs_write:
            self.save_policy(policy)
        return policy, changed

    def add_endless_account(self, account_id: int) -> tuple[dict[str, Any], bool]:
        policy = dict(self.policy_config())
        whitelist = set(normalize_account_ids(policy.get("whitelist_account_ids")))
        current = set(normalize_account_ids(policy.get("endless_account_ids")))
        before = set(current)
        current.add(int(account_id))
        whitelist.discard(int(account_id))
        normalized = sorted(current)
        normalized_whitelist = sorted(whitelist)
        needs_write = policy.get("endless_account_ids") != normalized or policy.get("whitelist_account_ids") != normalized_whitelist
        policy["whitelist_account_ids"] = normalized_whitelist
        policy["endless_account_ids"] = normalized
        changed = current != before or int(account_id) in set(normalize_account_ids(self.policy_config().get("whitelist_account_ids")))
        if changed or needs_write:
            self.save_policy(policy)
        return policy, changed

    def remove_endless_account(self, account_id: int) -> tuple[dict[str, Any], bool]:
        policy = dict(self.policy_config())
        current = set(normalize_account_ids(policy.get("endless_account_ids")))
        before = set(current)
        current.discard(int(account_id))
        normalized = sorted(current)
        needs_write = policy.get("endless_account_ids") != normalized
        policy["endless_account_ids"] = normalized
        if "whitelist_account_ids" in policy:
            policy["whitelist_account_ids"] = sorted(normalize_account_ids(policy.get("whitelist_account_ids")))
        changed = current != before
        if changed or needs_write:
            self.save_policy(policy)
        return policy, changed

    def clear_whitelist_accounts(self) -> tuple[dict[str, Any], bool]:
        policy = dict(self.policy_config())
        current = normalize_account_ids(policy.get("whitelist_account_ids"))
        policy["whitelist_account_ids"] = []
        if "endless_account_ids" in policy:
            policy["endless_account_ids"] = sorted(normalize_account_ids(policy.get("endless_account_ids")))
        changed = bool(current)
        if changed or self.policy_config().get("whitelist_account_ids") != []:
            self.save_policy(policy)
        return policy, changed
