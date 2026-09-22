from __future__ import annotations

import json
import os
import tempfile
import unittest
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("OPS_SESSION_SECRET", "test-session-secret")
os.environ.setdefault("DATABASE_URL", "postgresql://user:pass@127.0.0.1:5432/db")

from fastapi.datastructures import FormData
from app import main as main_module

from app.model_guard import (
    ModelGuard,
    PriceCatalog,
    classify_model_event,
    compare_openai_models,
    normalize_model,
    precise_mapping_key,
    remove_mapping_transaction,
)


class ModelGuardUnitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.catalog = PriceCatalog.from_payload(
            {
                "source_sha256": "price-sha",
                "prices": {
                    "gpt-6-astra": {"input_price": 10, "output_price": 50, "unit": "token"},
                    "gpt-5.6-sol": {"input_price": 5, "output_price": 30, "unit": "token"},
                    "gpt-5.6-luna": {"input_price": 2, "output_price": 12, "unit": "token"},
                },
            }
        )

    def test_date_aliases_are_normalized(self) -> None:
        self.assertEqual(normalize_model("gpt-6-astra@2026-09-21"), "gpt-6-astra")
        self.assertEqual(normalize_model("gpt-6-astra:20260921"), "gpt-6-astra")
        self.assertEqual(normalize_model("gpt-6-astra-20260921"), "gpt-6-astra")

    def test_openai_rank_beats_missing_or_conflicting_prices(self) -> None:
        no_prices = PriceCatalog({})
        self.assertEqual(compare_openai_models("gpt-6-sol", "gpt-5.6-sol", no_prices)[0], "confirmed")
        self.assertEqual(compare_openai_models("gpt-6-astra", "gpt-5.6-sol", no_prices)[0], "confirmed")
        self.assertEqual(compare_openai_models("gpt-6-luna", "gpt-5.6-astra", no_prices)[0], "ok")
        self.assertEqual(compare_openai_models("gpt-5.6-luna", "gpt-5.6-terra", no_prices)[0], "ok")
        self.assertEqual(compare_openai_models("gpt-6-sol@2026-09-21", "gpt-5.6-sol", no_prices)[0], "confirmed")
        expensive_response = PriceCatalog({
            "gpt-6-sol": self.catalog.prices["gpt-5.6-luna"],
            "gpt-5.6-sol": self.catalog.prices["gpt-6-astra"],
        })
        self.assertEqual(compare_openai_models("gpt-6-sol", "gpt-5.6-sol", expensive_response)[0], "confirmed")

    def test_grok_keeps_price_rule_and_other_openai_models_fall_back(self) -> None:
        log = {"id": 1, "account_id": 7, "requested_model": "gpt-6-sol", "upstream_model": "gpt-6-sol", "upstream_response_model": "gpt-5.6-sol"}
        self.assertEqual(classify_model_event({**log, "platform": "openai"}, PriceCatalog({}))["status"], "confirmed")
        self.assertEqual(classify_model_event({**log, "platform": "grok"}, PriceCatalog({}))["status"], "unconfirmed")
        self.assertEqual(compare_openai_models("o4-large", "o4-small", PriceCatalog({}))[0], "unconfirmed")

    def test_further_downgrade_is_confirmed(self) -> None:
        result = classify_model_event(
            {"id": 10, "account_id": 7, "requested_model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol", "upstream_response_model": "gpt-5.6-luna"},
            self.catalog,
        )
        self.assertEqual(result["status"], "confirmed")

    def test_intended_mapping_is_not_anomaly(self) -> None:
        result = classify_model_event(
            {"id": 10, "account_id": 7, "requested_model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol", "upstream_response_model": "gpt-5.6-sol"},
            self.catalog,
        )
        self.assertEqual(result["status"], "ok")

    def test_unknown_and_missing_response_are_distinct(self) -> None:
        unknown = classify_model_event(
            {"id": 1, "account_id": 7, "model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol", "upstream_response_model": "mystery"},
            self.catalog,
        )
        missing = classify_model_event(
            {"id": 2, "account_id": 7, "model": "gpt-6-astra", "upstream_model": "gpt-5.6-sol"},
            self.catalog,
        )
        self.assertEqual(unknown["status"], "unconfirmed")
        self.assertEqual(missing["status"], "missing")

    def test_missing_upstream_model_never_confirms_downgrade(self) -> None:
        result = classify_model_event(
            {"id": 3, "account_id": 7, "model": "gpt-6-astra", "upstream_response_model": "gpt-5.6-luna"},
            self.catalog,
        )
        self.assertEqual(result["status"], "unconfirmed")

    def test_precise_mapping_fails_closed_for_wildcard_or_last_entry(self) -> None:
        row = {"credentials": {"model_mapping": {"gpt-6-astra": "gpt-5.6-sol", "gpt-5.6-sol": "gpt-5.6-sol"}}}
        self.assertEqual(precise_mapping_key(row, "gpt-6-astra", "gpt-5.6-sol"), "gpt-6-astra")
        self.assertIsNone(precise_mapping_key({"credentials": {"model_mapping": {"gpt-*": "gpt-5.6-sol", "x": "y"}}}, "gpt-6-astra", "gpt-5.6-sol"))
        self.assertIsNone(precise_mapping_key({"credentials": {"model_mapping": {"gpt-6-astra": "gpt-5.6-sol"}}}, "gpt-6-astra", "gpt-5.6-sol"))
        self.assertIsNone(precise_mapping_key({"credentials": {"model_mapping": {"gpt-6-astra": "gpt-5.6-sol", "gpt-*": "gpt-5.6-luna"}}}, "gpt-6-astra", "gpt-5.6-sol"))

    def test_disabled_guard_does_not_query_logs(self) -> None:
        class DB:
            def fetch_all(self, *_args, **_kwargs):
                raise AssertionError("disabled guard queried logs")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            settings = SimpleNamespace(
                model_guard_config_path=str(root / "config.json"),
                model_guard_state_path=str(root / "state.json"),
                model_guard_pricing_path=str(root / "pricing.json"),
                audit_path=str(root / "audit.jsonl"),
            )
            controller = ModelGuard(settings, DB(), catalog=self.catalog)
            self.assertTrue(controller.run_once()["skipped"])


class FakeLogDB:
    def __init__(self, logs: list[dict[str, object]] | None = None) -> None:
        self.logs = logs or []
        self.queries: list[dict[str, object]] = []

    def fetch_all(self, _sql: str, params: dict[str, object]) -> list[dict[str, object]]:
        self.queries.append(dict(params))
        rows = list(self.logs)
        if "platform" in params:
            rows = [row for row in rows if row["platform"] == params["platform"]]
        if "legacy_account_id" in params:
            rows = [row for row in rows if row["account_id"] == params["legacy_account_id"] and row["requested_model"] == params["requested"]]
        rows = [row for row in rows if int(row["id"]) > int(params.get("after", params.get("cursor", 0)))]
        if "cursor" in params and "after" in params:
            rows = [row for row in rows if int(row["id"]) <= int(params["cursor"])]
        if "upper_id" in params:
            rows = [row for row in rows if int(row["id"]) <= int(params["upper_id"])]
        if "since" in params:
            rows = [row for row in rows if row["created_at"] >= params["since"]]
        return sorted(rows, key=lambda row: int(row["id"]))[:int(params["limit"])]

    def fetch_one(self, _sql: str, params: dict[str, object]) -> dict[str, int]:
        return {"id": max((int(row["id"]) for row in self.logs if row["platform"] == params["platform"]), default=0)}


class ModelGuardFlowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.settings = SimpleNamespace(
            model_guard_config_path=str(root / "config.json"),
            model_guard_state_path=str(root / "state.json"),
            model_guard_pricing_path=str(root / "pricing.json"),
            audit_path=str(root / "audit.jsonl"),
        )
        self.now = datetime.now(timezone.utc)
        self.db = FakeLogDB()
        self.accounts = {
            1: {"id": 1, "name": "OpenAI", "platform": "openai", "type": "oauth", "credentials": {"model_mapping": {"gpt-6-sol": "gpt-6-sol", "other": "other"}}},
            2: {"id": 2, "name": "Grok", "platform": "grok", "type": "apikey", "credentials": {"model_mapping": {"grok-high": "grok-high", "other": "other"}}},
        }
        self.catalog = PriceCatalog.from_payload({"prices": {"grok-high": {"input_price": 10, "output_price": 20}, "grok-low": {"input_price": 5, "output_price": 10}}})
        self.guard = ModelGuard(self.settings, self.db, catalog=self.catalog, account_reader=lambda _db, account_id: self.accounts.get(account_id))

    def add_log(self, log_id: int, platform: str, *, created_at: datetime | None = None) -> None:
        upstream, response, account_id = ("gpt-6-sol", "gpt-5.6-sol", 1) if platform == "openai" else ("grok-high", "grok-low", 2)
        self.db.logs.append({"id": log_id, "account_id": account_id, "platform": platform, "requested_model": upstream, "model": upstream, "upstream_model": upstream, "upstream_response_model": response, "model_mapping_chain": f"{upstream}→{upstream}", "inbound_endpoint": "/responses", "created_at": created_at or self.now})

    def test_all_four_switch_combinations_isolate_platforms(self) -> None:
        for openai_enabled, grok_enabled in ((False, False), (True, False), (False, True), (True, True)):
            with self.subTest(openai=openai_enabled, grok=grok_enabled):
                with tempfile.TemporaryDirectory() as directory:
                    settings = SimpleNamespace(**{**vars(self.settings), "model_guard_config_path": f"{directory}/config.json", "model_guard_state_path": f"{directory}/state.json", "audit_path": f"{directory}/audit.jsonl"})
                    db = FakeLogDB()
                    guard = ModelGuard(settings, db, catalog=self.catalog, account_reader=lambda _db, account_id: self.accounts.get(account_id))
                    guard.save_config(openai_enabled=openai_enabled, grok_enabled=grok_enabled, auto_remove=False, user="tester")
                    guard.run_once(self.now)
                    db.logs.extend(self.db.logs)
                    db.logs.extend([{
                        "id": log_id, "account_id": account_id, "platform": platform, "requested_model": upstream,
                        "upstream_model": upstream, "upstream_response_model": response, "created_at": self.now,
                    } for log_id, account_id, platform, upstream, response in ((1, 1, "openai", "gpt-6-sol", "gpt-5.6-sol"), (2, 2, "grok", "grok-high", "grok-low"))])
                    guard.run_once(self.now + timedelta(seconds=1))
                    incidents = guard.panel_snapshot()["incidents"]
                    self.assertEqual({item["platform"] for item in incidents}, {p for p, flag in (("openai", openai_enabled), ("grok", grok_enabled)) if flag})

    def test_legacy_config_migrates_with_permissions_and_preserves_auto_remove(self) -> None:
        Path(self.settings.model_guard_config_path).write_text(json.dumps({"enabled": True, "auto_remove": True, "config_version": 4}))
        Path(self.settings.model_guard_state_path).write_text(json.dumps({"cursor": 42, "history_bootstrapped": True, "seen": [], "incidents": {}, "pending_events": []}))
        self.assertTrue(self.guard.config().openai_enabled)
        self.assertTrue(self.guard.config().grok_enabled)
        self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=True, user="tester")
        data = json.loads(Path(self.settings.model_guard_config_path).read_text())
        self.assertNotIn("enabled", data)
        self.assertEqual(data["config_version"], 5)
        self.assertEqual((data["openai_enabled"], data["grok_enabled"], data["auto_remove"]), (True, False, True))
        self.assertEqual(Path(self.settings.model_guard_config_path).stat().st_mode & 0o777, 0o600)
        state = json.loads(Path(self.settings.model_guard_state_path).read_text())
        self.assertEqual(state["platforms"]["openai"]["cursor"], 42)
        self.assertEqual(state["platforms"]["grok"]["cursor"], 42)

    def test_disabled_period_is_history_only_on_reenable(self) -> None:
        with patch("app.model_guard.remove_mapping_transaction") as remove:
            self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=True, user="tester")
            self.guard.run_once(self.now)
            self.guard.save_config(openai_enabled=False, grok_enabled=False, auto_remove=True, user="tester")
            self.add_log(1, "openai", created_at=self.now + timedelta(seconds=1))
            self.guard.run_once(self.now + timedelta(seconds=2))
            self.assertEqual(self.guard.panel_snapshot()["incidents"], [])
            self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=True, user="tester")
            self.guard.run_once(self.now + timedelta(seconds=3))
            self.assertEqual(len(self.guard.panel_snapshot()["incidents"]), 1)
            self.assertTrue(self.guard.panel_snapshot()["incidents"][0]["history"])
            self.assertEqual(len(self.guard.pending_events()), 1)
            remove.assert_not_called()
            restarted = ModelGuard(self.settings, self.db, catalog=self.catalog, account_reader=lambda _db, account_id: self.accounts.get(account_id))
            restarted.run_once(self.now + timedelta(seconds=4))
            self.assertEqual(len(restarted.pending_events()), 1)

    def test_auto_remove_only_runs_for_enabled_platform(self) -> None:
        self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=True, user="tester")
        self.guard.run_once(self.now)
        self.add_log(1, "openai", created_at=self.now + timedelta(seconds=1))
        self.add_log(2, "grok", created_at=self.now + timedelta(seconds=1))
        with patch("app.model_guard.remove_mapping_transaction", return_value=(True, "已移除精确模型入口")) as remove:
            self.guard.run_once(self.now + timedelta(seconds=2))
            self.assertEqual(remove.call_count, 1)
            self.assertEqual(remove.call_args.args[1]["platform"], "openai")
        self.assertEqual({item["platform"] for item in self.guard.panel_snapshot()["incidents"]}, {"openai"})

    def test_late_committed_log_is_found_once_by_overlap(self) -> None:
        self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=False, user="tester")
        self.guard.run_once(self.now)
        self.add_log(100, "openai", created_at=self.now + timedelta(seconds=1))
        self.guard.run_once(self.now + timedelta(seconds=2))
        self.add_log(50, "openai", created_at=self.now + timedelta(seconds=3))
        self.guard.run_once(self.now + timedelta(seconds=4))
        self.guard.run_once(self.now + timedelta(seconds=5))
        state = json.loads(Path(self.settings.model_guard_state_path).read_text())
        self.assertEqual(state["platforms"]["openai"]["coverage"]["total"], 2)
        self.assertEqual(self.guard.panel_snapshot()["incidents"][0]["count"], 2)

    def test_legacy_unconfirmed_reclassification_queues_one_event_without_action(self) -> None:
        Path(self.settings.model_guard_config_path).write_text(json.dumps({"enabled": True, "auto_remove": True, "config_version": 1}))
        Path(self.settings.model_guard_state_path).write_text(json.dumps({
            "cursor": 8, "history_bootstrapped": True, "incidents": {
                "1:gpt-6-sol": {"account_id": 1, "account_name": "OpenAI", "account_type": "oauth", "platform": "openai", "requested_model": "gpt-6-sol", "upstream_model": "gpt-6-sol", "response_model": "gpt-5.6-sol", "log_id": 8, "created_at": self.now.isoformat(), "latest_at": self.now.isoformat(), "status": "unconfirmed", "action": "待核实", "count": 3}
            }, "pending_events": [], "seen": [8]
        }))
        with patch("app.model_guard.remove_mapping_transaction") as remove:
            self.guard.run_once(self.now)
            restarted = ModelGuard(self.settings, self.db, catalog=self.catalog, account_reader=lambda _db, account_id: self.accounts.get(account_id))
            restarted.run_once(self.now + timedelta(seconds=1))
            remove.assert_not_called()
        incidents = self.guard.panel_snapshot()["incidents"]
        self.assertEqual(len(incidents), 1)
        self.assertEqual(incidents[0]["status"], "confirmed")
        self.assertEqual(incidents[0]["action"], "历史重判仅告警")
        events = self.guard.pending_events()
        self.assertEqual(len(events), 1)
        self.guard.mark_events_delivered(events)
        self.assertEqual(self.guard.pending_events(), [])

    def test_legacy_mixed_chain_rebuild_keeps_downgrade_not_upgrade(self) -> None:
        Path(self.settings.model_guard_config_path).write_text(json.dumps({"enabled": True, "auto_remove": True, "config_version": 1}))
        Path(self.settings.model_guard_state_path).write_text(json.dumps({
            "cursor": 2, "history_bootstrapped": True, "incidents": {
                "1:gpt-6-astra": {"account_id": 1, "account_name": "OpenAI", "account_type": "oauth", "platform": "openai", "requested_model": "gpt-6-astra", "upstream_model": "gpt-5.6-luna", "response_model": "gpt-5.6-terra", "log_id": 2, "latest_at": self.now.isoformat(), "status": "confirmed", "action": "待核实", "count": 2}
            }, "pending_events": [], "seen": [1, 2]
        }))
        self.db.logs.extend([
            {"id": 1, "account_id": 1, "platform": "openai", "requested_model": "gpt-6-astra", "upstream_model": "gpt-6-astra", "upstream_response_model": "gpt-5.6-sol", "created_at": self.now - timedelta(seconds=1)},
            {"id": 2, "account_id": 1, "platform": "openai", "requested_model": "gpt-6-astra", "upstream_model": "gpt-5.6-luna", "upstream_response_model": "gpt-5.6-terra", "created_at": self.now},
        ])
        with patch("app.model_guard.remove_mapping_transaction") as remove:
            self.guard.run_once(self.now)
            remove.assert_not_called()
        incidents = self.guard.panel_snapshot()["incidents"]
        self.assertEqual(len(incidents), 1)
        self.assertEqual(incidents[0]["upstream_model"], "gpt-6-astra")
        self.assertEqual(incidents[0]["response_model"], "gpt-5.6-sol")
        self.assertEqual(incidents[0]["status"], "confirmed")
        self.assertIn("1:gpt-6-astra", json.loads(Path(self.settings.model_guard_state_path).read_text())["legacy_incidents"])

    def test_real_migrated_history_sends_one_bark_summary_per_account(self) -> None:
        self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=False, user="tester")
        state = json.loads(Path(self.settings.model_guard_state_path).read_text())
        state.update({"rule_version": 2, "legacy_incidents": {"old:1": {"account_id": 1}}, "incidents": {
            "chain:a": {"account_id": 1, "account_name": "OpenAI", "platform": "openai", "account_type": "oauth", "requested_model": "gpt-6-astra", "upstream_model": "gpt-6-astra", "response_model": "gpt-5.6-luna", "latest_at": self.now.isoformat(), "count": 2, "history": True, "status": "confirmed", "action": "历史仅告警", "log_id": 10},
            "chain:b": {"account_id": 1, "account_name": "OpenAI", "platform": "openai", "account_type": "oauth", "requested_model": "gpt-6-sol", "upstream_model": "gpt-6-sol", "response_model": "gpt-5.6-sol", "latest_at": self.now.isoformat(), "count": 3, "history": True, "status": "confirmed", "action": "历史仅告警", "log_id": 11},
        }})
        Path(self.settings.model_guard_state_path).write_text(json.dumps(state))
        self.guard.run_once(self.now)
        events = self.guard.pending_events()
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["kind"], "history_summary")
        self.assertEqual(events[0]["count"], 5)
        self.guard.run_once(self.now + timedelta(seconds=1))
        self.assertEqual(len(self.guard.pending_events()), 1)
        self.guard.mark_events_delivered(events)
        self.guard.run_once(self.now + timedelta(seconds=2))
        self.assertEqual(self.guard.pending_events(), [])

    def test_history_pagination_sends_one_account_summary(self) -> None:
        self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=False, user="tester")
        for log_id in range(1, 502):
            self.add_log(log_id, "openai")
        self.guard.run_once(self.now)
        self.assertEqual(len(self.guard.pending_events()), 0)
        self.guard.run_once(self.now + timedelta(seconds=1))
        events = self.guard.pending_events()
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["count"], 501)
        self.assertEqual(self.guard.panel_snapshot()["incidents"][0]["count"], 501)

    def test_claim_survives_crash_and_never_replays_account_update(self) -> None:
        self.guard.save_config(openai_enabled=True, grok_enabled=False, auto_remove=True, user="tester")
        self.guard.run_once(self.now)
        self.add_log(1, "openai", created_at=self.now + timedelta(seconds=1))
        with patch("app.model_guard.remove_mapping_transaction", side_effect=SystemExit):
            with self.assertRaises(SystemExit):
                self.guard.run_once(self.now + timedelta(seconds=2))
        restarted = ModelGuard(self.settings, self.db, catalog=self.catalog, account_reader=lambda _db, account_id: self.accounts.get(account_id))
        with patch("app.model_guard.remove_mapping_transaction") as remove:
            restarted.run_once(self.now + timedelta(seconds=3))
            remove.assert_not_called()
        self.assertEqual(restarted.panel_snapshot()["incidents"][0]["action"], "处置结果待核实")
        self.assertEqual(len(restarted.pending_events()), 1)

    def test_delivery_acknowledges_exact_event_only(self) -> None:
        state = {"pending_events": [{"event_id": "old:1", "log_id": 1}, {"event_id": "new:1", "log_id": 1}], "incidents": {}, "rule_version": 2}
        Path(self.settings.model_guard_state_path).write_text(json.dumps(state))
        self.guard.mark_events_delivered([state["pending_events"][0]])
        self.assertEqual([item["event_id"] for item in self.guard.pending_events()], ["new:1"])


class TransactionDB:
    def __init__(self, row: dict[str, object], *, fail_outbox: bool = False, fail_update: bool = False) -> None:
        self.row = row
        self.outbox: list[dict[str, object]] = []
        self.fail_outbox = fail_outbox
        self.fail_update = fail_update
        self.pending_row: dict[str, object] | None = None
        self.pending_outbox: list[dict[str, object]] = []
        self.rowcount = 0

    @contextmanager
    def connection(self):
        yield self

    @contextmanager
    def transaction(self):
        self.pending_row = json.loads(json.dumps(self.row))
        self.pending_outbox = []
        try:
            yield self
        except Exception:
            self.pending_row = None
            self.pending_outbox = []
            raise
        else:
            self.row = self.pending_row
            self.outbox.extend(self.pending_outbox)

    @contextmanager
    def cursor(self):
        yield self

    def execute(self, sql: str, params: dict[str, object] | None = None) -> None:
        params = params or {}
        if sql.startswith("UPDATE accounts"):
            self.rowcount = 0 if self.fail_update else 1
            if self.rowcount:
                self.pending_row["credentials"]["model_mapping"].pop(params["key"])
        elif sql.startswith("INSERT INTO scheduler_outbox"):
            if self.fail_outbox:
                raise RuntimeError("outbox failure")
            self.pending_outbox.append(params)

    def fetchone(self) -> dict[str, object]:
        return self.pending_row


class ModelGuardTransactionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.row = {"id": 7, "platform": "openai", "type": "oauth", "credentials": {"model_mapping": {"gpt-6-astra": "gpt-6-astra", "other": "other"}}, "extra": {}}
        self.evidence = {"account_id": 7, "requested_model": "gpt-6-astra", "upstream_model": "gpt-6-astra", "inbound_endpoint": "/responses", "model_mapping_chain": "gpt-6-astra→gpt-6-astra"}

    def test_exact_mapping_and_outbox_commit_together(self) -> None:
        db = TransactionDB(json.loads(json.dumps(self.row)))
        result = remove_mapping_transaction(db, self.evidence, self.row)
        self.assertTrue(result[0])
        self.assertEqual(db.row["credentials"]["model_mapping"], {"other": "other"})
        self.assertEqual(len(db.outbox), 1)
        self.assertEqual(db.outbox[0]["event_type"], "account_changed")

    def test_failed_outbox_or_conditional_update_cannot_commit_mapping(self) -> None:
        for options in ({"fail_outbox": True}, {"fail_update": True}):
            with self.subTest(options=options):
                db = TransactionDB(json.loads(json.dumps(self.row)), **options)
                result = remove_mapping_transaction(db, self.evidence, self.row)
                self.assertFalse(result[0])
                self.assertEqual(db.row["credentials"]["model_mapping"], self.row["credentials"]["model_mapping"])
                self.assertEqual(db.outbox, [])


class ModelGuardRouteTests(unittest.IsolatedAsyncioTestCase):
    async def test_route_requires_auth_and_saves_split_switches(self) -> None:
        route = next(item for item in main_module.app.routes if getattr(item, "path", "") == "/model-guard/config")
        self.assertIn("require_auth", [dep.call.__name__ for dep in route.dependant.dependencies])

        class FormRequest:
            async def form(self) -> FormData:
                return FormData([("openai_enabled", "1"), ("auto_remove", "1")])

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            settings = SimpleNamespace(model_guard_config_path=str(root / "config.json"), model_guard_state_path=str(root / "state.json"), model_guard_pricing_path=str(root / "pricing.json"), audit_path=str(root / "audit.jsonl"), base_path="/sub2ops")
            guard = ModelGuard(settings, FakeLogDB(), catalog=PriceCatalog({}))
            previous_guard, previous_settings = main_module.model_guard, main_module.settings
            main_module.model_guard, main_module.settings = guard, settings
            try:
                response = await main_module.model_guard_config_save(FormRequest(), "admin")
            finally:
                main_module.model_guard, main_module.settings = previous_guard, previous_settings
            self.assertEqual(response.status_code, 303)
            self.assertTrue(guard.config().openai_enabled)
            self.assertFalse(guard.config().grok_enabled)
            self.assertTrue(guard.config().auto_remove)

    def test_template_uses_platform_switches(self) -> None:
        template = (Path(__file__).resolve().parents[1] / "app/templates/telegram.html").read_text()
        self.assertIn('name="openai_enabled"', template)
        self.assertIn('name="grok_enabled"', template)
        self.assertNotIn('name="enabled" value="1" {{ \'checked\' if guard.enabled', template)


if __name__ == "__main__":
    unittest.main()
