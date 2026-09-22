from __future__ import annotations

import json
import os
import unittest
from contextlib import contextmanager

import psycopg
from psycopg.rows import dict_row

from app.model_guard import remove_mapping_transaction


class SingleConnectionDB:
    def __init__(self, connection: psycopg.Connection) -> None:
        self._connection = connection

    @contextmanager
    def connection(self):
        yield self._connection


@unittest.skipUnless(os.getenv("MODEL_GUARD_POSTGRES_TEST_URL"), "isolated PostgreSQL URL not configured")
class ModelGuardPostgresTests(unittest.TestCase):
    def setUp(self) -> None:
        self.connection = psycopg.connect(os.environ["MODEL_GUARD_POSTGRES_TEST_URL"], row_factory=dict_row)
        self.addCleanup(self.connection.close)
        self.connection.execute(
            "CREATE TEMP TABLE accounts (id bigint PRIMARY KEY, platform text, type text, "
            "credentials jsonb, extra jsonb, deleted_at timestamptz, updated_at timestamptz)"
        )
        self.connection.execute(
            "CREATE TEMP TABLE scheduler_outbox (event_type text, account_id bigint, payload jsonb)"
        )
        self.connection.commit()

    def insert_account(self, account_id: int) -> dict:
        row = {
            "id": account_id, "platform": "openai", "type": "oauth",
            "credentials": {"model_mapping": {"gpt-6-astra": "gpt-6-astra", "gpt-5.6-sol": "gpt-5.6-sol"}, "qa_marker": "preserve"},
            "extra": {"qa_marker": "preserve"},
        }
        self.connection.execute(
            "INSERT INTO accounts (id, platform, type, credentials, extra) "
            "VALUES (%s, %s, %s, %s::jsonb, %s::jsonb)",
            (account_id, row["platform"], row["type"], json.dumps(row["credentials"]), json.dumps(row["extra"])),
        )
        self.connection.commit()
        return row

    def test_exact_delete_and_outbox_share_one_transaction(self) -> None:
        row = self.insert_account(101)
        evidence = {
            "account_id": 101, "requested_model": "gpt-6-astra", "upstream_model": "gpt-6-astra",
            "model_mapping_chain": "gpt-6-astra→gpt-6-astra", "inbound_endpoint": "/responses",
        }
        self.assertTrue(remove_mapping_transaction(SingleConnectionDB(self.connection), evidence, row)[0])
        updated = self.connection.execute("SELECT credentials, extra FROM accounts WHERE id = 101").fetchone()
        event = self.connection.execute("SELECT event_type, payload FROM scheduler_outbox WHERE account_id = 101").fetchone()
        self.assertEqual(updated["credentials"]["model_mapping"], {"gpt-5.6-sol": "gpt-5.6-sol"})
        self.assertEqual(updated["credentials"]["qa_marker"], "preserve")
        self.assertEqual(updated["extra"], {"qa_marker": "preserve"})
        self.assertEqual(event["event_type"], "account_changed")
        self.assertEqual(event["payload"]["model"], "gpt-6-astra")
        self.connection.commit()

        self.connection.execute("TRUNCATE scheduler_outbox")
        self.connection.execute("ALTER TABLE scheduler_outbox ADD CONSTRAINT reject_qa_event CHECK (event_type <> 'account_changed')")
        self.connection.commit()
        second_row = self.insert_account(102)
        evidence["account_id"] = 102
        self.assertFalse(remove_mapping_transaction(SingleConnectionDB(self.connection), evidence, second_row)[0])
        unchanged = self.connection.execute("SELECT credentials FROM accounts WHERE id = 102").fetchone()
        outbox_count = self.connection.execute("SELECT count(*) AS total FROM scheduler_outbox").fetchone()
        self.assertEqual(unchanged["credentials"], second_row["credentials"])
        self.assertEqual(outbox_count["total"], 0)
