from __future__ import annotations

import unittest
from datetime import datetime, timezone
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any

from app.guard_engine import GuardEngine
from app.guard_policy import GuardPolicy
from app.guard_store import GuardStore
from app.sql import GUARD_SUCCESS_EVENTS_SQL


class FakeDB:
    def __init__(self, error_rows: list[dict[str, Any]], success_rows: list[dict[str, Any]] | None = None) -> None:
        self.error_rows = error_rows
        self.success_rows = success_rows or []
        self.queries: list[tuple[str, dict[str, Any]]] = []
        self.updates: list[tuple[str, dict[str, Any]]] = []

    def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        payload = params or {}
        self.queries.append((sql, payload))
        if sql == GUARD_SUCCESS_EVENTS_SQL:
            return list(self.success_rows)
        return list(self.error_rows)

    def fetch_one(self, sql: str, params: dict[str, Any] | None = None) -> dict[str, Any] | None:
        payload = params or {}
        self.updates.append((sql, payload))
        if "UPDATE accounts" in sql:
            return {"id": payload["account_id"], "name": "wong", "schedulable": False}
        return None


def row(**overrides: object) -> dict[str, object]:
    base: dict[str, object] = {
        "error_log_id": 101,
        "attempt_no": 1,
        "created_at": datetime(2026, 5, 18, 10, 0, tzinfo=timezone.utc),
        "account_id": 9,
        "account_name": "wong",
        "account_priority": 50,
        "status_code": 403,
        "kind": "http_error",
        "error_owner": "provider",
        "error_source": "upstream",
        "message": "token quota is not enough",
        "search_text": "pre_consume_token_quota_failed token quota is not enough",
    }
    base.update(overrides)
    return base


class GuardEngineTests(unittest.TestCase):
    def test_engine_pauses_quota_fault_and_advances_cursor(self) -> None:
        with TemporaryDirectory() as tmp:
            db = FakeDB([row()])
            store = GuardStore(str(Path(tmp) / "state.json"))
            engine = GuardEngine(db=db, store=store, audit_path=str(Path(tmp) / "audit.jsonl"), policy=GuardPolicy())
            actions = engine.run_once(actor="test")

            self.assertEqual(len(actions), 1)
            self.assertEqual(actions[0]["action"], "pause")
            self.assertEqual(actions[0]["account_id"], 9)
            self.assertEqual(store.error_cursor(), 101)

    def test_engine_ignores_client_error_but_advances_cursor(self) -> None:
        with TemporaryDirectory() as tmp:
            db = FakeDB([row(error_owner="client", error_source="client_request", search_text="bad user input")])
            store = GuardStore(str(Path(tmp) / "state.json"))
            engine = GuardEngine(db=db, store=store, audit_path=str(Path(tmp) / "audit.jsonl"), policy=GuardPolicy())
            actions = engine.run_once(actor="test")

            self.assertEqual(actions, [])
            self.assertEqual(store.error_cursor(), 101)
            self.assertEqual(db.updates, [])

    def test_rate_limit_soft_landing_writes_load_factor_when_supported(self) -> None:
        with TemporaryDirectory() as tmp:
            db = FakeDB([row(status_code=429, message="rate limit", search_text="rate limit")])
            store = GuardStore(str(Path(tmp) / "state.json"))
            engine = GuardEngine(
                db=db,
                store=store,
                audit_path=str(Path(tmp) / "audit.jsonl"),
                policy=GuardPolicy(),
                load_factor_supported=True,
            )
            actions = engine.run_once(actor="test")

            self.assertEqual(actions[0]["action"], "cooldown")
            self.assertEqual(actions[0]["load_factor"], 1)
            self.assertTrue(any(params.get("load_factor") == 1 for _sql, params in db.updates))

    def test_record_recovery_success_closes_open_circuit(self) -> None:
        with TemporaryDirectory() as tmp:
            store = GuardStore(str(Path(tmp) / "state.json"))
            store.save_circuit(store.circuit(9))
            circuit = store.circuit(9)
            circuit.state = "open"
            circuit.consecutive_failures = 4
            store.save_circuit(circuit)

            engine = GuardEngine(FakeDB([]), store, str(Path(tmp) / "audit.jsonl"), GuardPolicy(success_threshold=1))
            engine.record_recovery_success(9, 777)

            self.assertEqual(store.circuit(9).state, "closed")

    def test_duplicate_recovery_success_does_not_reopen_closed_circuit(self) -> None:
        with TemporaryDirectory() as tmp:
            store = GuardStore(str(Path(tmp) / "state.json"))
            circuit = store.circuit(9)
            circuit.state = "open"
            circuit.consecutive_failures = 4
            store.save_circuit(circuit)

            engine = GuardEngine(FakeDB([]), store, str(Path(tmp) / "audit.jsonl"), GuardPolicy(success_threshold=1))
            engine.record_recovery_success(9, 777)
            self.assertEqual(store.circuit(9).state, "closed")

            engine.record_recovery_success(9, 777)

            self.assertEqual(store.circuit(9).state, "closed")


if __name__ == "__main__":
    unittest.main()
