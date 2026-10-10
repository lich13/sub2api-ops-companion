from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import re
import sqlite3
import tempfile
import threading
import unittest
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from decimal import Decimal, localcontext
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.desktop_api import install_desktop_api
from app.usage_records import UsageRecords, project, timestamp


NOW = datetime(2026, 9, 30, 12, tzinfo=timezone.utc)
PATH = "/api/desktop/v1/usage-records"
OPTIONS_PATH = "/api/desktop/v1/usage-record-options"


class FixedDateTime(datetime):
    current = NOW

    @classmethod
    def now(cls, tz=None):
        return cls.fromisoformat(cls.current.astimezone(tz or timezone.utc).isoformat())


class DecimalSum:
    """Preserve NUMERIC addition when SQLite fixtures store money as TEXT."""

    def __init__(self):
        self.values = []

    def step(self, value):
        if value is not None:
            self.values.append(Decimal(str(value)))

    def finalize(self):
        if not self.values:
            return None
        with localcontext() as context:
            highest = max(value.adjusted() for value in self.values)
            lowest = min(value.as_tuple().exponent for value in self.values)
            context.prec = max(28, highest - lowest + len(str(len(self.values))) + 2)
            return str(sum(self.values, Decimal(0)))


class ReadOnlyFixtureDB:
    """Execute SELECT semantics in SQLite; check the PostgreSQL transaction contract.

    Translate named parameters and ILIKE; register exact NUMERIC-like SUM.
    Filtering and pagination execute as SQL. This is not PostgreSQL acceptance.
    """

    def __init__(self, database=":memory:"):
        self.raw = sqlite3.connect(database, check_same_thread=False, isolation_level=None)
        self.raw.row_factory = sqlite3.Row
        self.raw.create_aggregate("sum", 1, DecimalSum)
        integers = """id user_id api_key_id account_id group_id upstream_model_mismatch
            request_type stream openai_ws_mode input_tokens output_tokens cache_creation_tokens
            cache_read_tokens cache_creation_5m_tokens cache_creation_1h_tokens first_token_ms
            duration_ms billing_type image_count image_output_tokens image_input_tokens video_count""".split()
        texts = """request_id upstream_request_id model requested_model upstream_model
            upstream_response_model model_mapping_chain reasoning_effort requested_reasoning_effort
            input_cost output_cost cache_creation_cost cache_read_cost total_cost actual_cost
            account_stats_cost account_rate_multiplier rate_multiplier created_at user_agent
            ip_address inbound_endpoint upstream_endpoint billing_mode service_tier
            image_output_cost image_input_cost video_duration_seconds video_resolution
            credentials request_headers response_headers request_body response_body""".split()
        columns = [f"{name} INTEGER" for name in integers] + [f"{name} TEXT" for name in texts]
        self.raw.execute("CREATE TABLE usage_logs (" + ",".join(columns) + ")")
        self.raw.executescript("""
            CREATE TABLE users (id INTEGER, username TEXT, email TEXT, status TEXT,
                                deleted_at TEXT, password TEXT, credentials TEXT);
            CREATE TABLE api_keys (id INTEGER, name TEXT, user_id INTEGER, status TEXT,
                                   deleted_at TEXT, key TEXT, token TEXT);
            CREATE TABLE accounts (id INTEGER, name TEXT, credentials TEXT, deleted_at TEXT);
            CREATE TABLE groups (id INTEGER, name TEXT);
            INSERT INTO users VALUES (3, '测试用户', 'test@example.invalid', 'active',
                                      NULL, 'PRIVATE_PASSWORD', 'PRIVATE_USER_CREDENTIALS');
            INSERT INTO api_keys VALUES (4, '测试 Key', 3, 'active',
                                         NULL, 'PRIVATE_API_KEY', 'PRIVATE_KEY_TOKEN');
            INSERT INTO accounts VALUES (7, '历史账号', 'PRIVATE_CREDENTIALS', '2026-09-01');
            INSERT INTO groups VALUES (8, '测试分组');
        """)
        self.statements = []
        self.transactions = []
        self.active = False
        self.read_only = False

    def close(self):
        self.raw.close()

    def insert(self, record_id, **changes):
        values = {"id": record_id, "created_at": (NOW - timedelta(minutes=5)).isoformat(),
                  "model": "gpt-test", "user_id": 3, "api_key_id": 4, "account_id": 7,
                  "group_id": 8, "credentials": "PRIVATE_ROW_CREDENTIALS",
                  "request_body": "PRIVATE_PROMPT", "response_body": "PRIVATE_COMPLETION",
                  "request_headers": "PRIVATE_AUTH_HEADER", **changes}
        values = {key: value.isoformat() if isinstance(value, datetime) else str(value)
                  if isinstance(value, Decimal) else value for key, value in values.items()}
        self.raw.execute("INSERT INTO usage_logs (" + ",".join(values) + ") VALUES (" +
                         ",".join("?" for _ in values) + ")", list(values.values()))

    def insert_user(self, user_id, **changes):
        values = {"id": user_id, "username": f"User {user_id}", "email": f"user{user_id}@example.invalid",
                  "status": "active", "deleted_at": None, "password": "PRIVATE_PASSWORD",
                  "credentials": "PRIVATE_USER_CREDENTIALS", **changes}
        self.raw.execute("INSERT INTO users (" + ",".join(values) + ") VALUES (" +
                         ",".join("?" for _ in values) + ")", list(values.values()))

    def insert_key(self, key_id, **changes):
        values = {"id": key_id, "name": f"Key {key_id}", "user_id": 3, "status": "active",
                  "deleted_at": None, "key": "PRIVATE_API_KEY", "token": "PRIVATE_KEY_TOKEN", **changes}
        self.raw.execute("INSERT INTO api_keys (" + ",".join(values) + ") VALUES (" +
                         ",".join("?" for _ in values) + ")", list(values.values()))

    @contextmanager
    def connection(self):
        yield self

    @contextmanager
    def transaction(self):
        if self.active:
            raise AssertionError("Unexpected nested transaction")
        self.active = True
        self.read_only = False
        self.transactions.append([])
        self.raw.execute("BEGIN")
        try:
            yield self
        finally:
            self.raw.execute("ROLLBACK")
            self.raw.execute("PRAGMA query_only = OFF")
            self.active = False
            self.read_only = False

    def execute(self, sql, params=None):
        if not self.active:
            raise AssertionError("Usage reads must be in an explicit transaction")
        self.statements.append((sql, dict(params or {})))
        self.transactions[-1].append(sql)
        normalized = " ".join(sql.upper().split())
        if normalized in ("SET TRANSACTION READ ONLY",
                          "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY"):
            self.read_only = True
            self.raw.execute("PRAGMA query_only = ON")
            return None
        if normalized == "SET LOCAL STATEMENT_TIMEOUT = '5S'":
            return None
        if not self.read_only or not normalized.startswith("SELECT "):
            raise AssertionError("Unexpected SQL or missing read-only transaction: " + sql)
        translated = re.sub(r"%\((\w+)\)s", r":\1", sql)
        translated = re.sub(r"ILIKE (:\w+)", r"LIKE \1 ESCAPE '\\'", translated)
        values = {key: value.isoformat() if isinstance(value, datetime) else value
                  for key, value in (params or {}).items()}
        cursor = self.raw.execute(translated, values)
        return SimpleNamespace(fetchone=lambda: self._dict(cursor.fetchone()),
                               fetchall=lambda: [dict(row) for row in cursor.fetchall()])

    @staticmethod
    def _dict(row):
        return None if row is None else dict(row)


class ProjectionTests(unittest.TestCase):
    def test_request_forwarded_and_response_models_remain_distinct(self):
        result = project({"model": "billing-model", "requested_model": "public-alias",
                          "upstream_model": "forwarded-model", "upstream_response_model": "returned-model",
                          "model_mapping_chain": "public-alias -> forwarded-model",
                          "requested_reasoning_effort": "high", "reasoning_effort": "medium"})
        self.assertEqual(result["requested_model"], "public-alias")
        self.assertEqual(result["upstream_model"], "forwarded-model")
        self.assertEqual(result["upstream_response_model"], "returned-model")
        self.assertEqual(result["model"], "billing-model")
        self.assertEqual(result["requested_reasoning_effort"], "high")
        self.assertEqual(result["reasoning_effort"], "medium")
        self.assertTrue(result["upstream_model_mismatch"])

    def test_mapping_itself_is_not_a_response_mismatch(self):
        result = project({"requested_model": "alias", "upstream_model": "actual",
                          "upstream_response_model": "actual"})
        self.assertFalse(result["upstream_model_mismatch"])

    def test_response_mismatch_uses_legacy_model_fallback_and_explicit_flag(self):
        cases = [({"model": "old", "upstream_response_model": "new"}, True),
                 ({"model": "old", "upstream_response_model": "old"}, False),
                 ({"model": "old", "upstream_response_model": ""}, False),
                 ({"upstream_response_model": "unknown-origin"}, False),
                 ({"model": "same", "upstream_response_model": "same", "upstream_model_mismatch": True}, True)]
        for row, expected in cases:
            with self.subTest(row=row):
                self.assertEqual(project(row)["upstream_model_mismatch"], expected)
        self.assertEqual(project({"model": "legacy", "requested_model": ""})["requested_model"], "legacy")

    def test_explicit_request_type_takes_precedence_over_legacy_flags(self):
        for value, expected in ((1, "sync"), (2, "stream"), (3, "ws_v2")):
            with self.subTest(value=value):
                self.assertEqual(project({"request_type": value, "stream": True,
                                          "openai_ws_mode": True})["request_type"], expected)

    def test_legacy_request_types_and_ws_precedence(self):
        for kind in (None, 0, -1, 99):
            for flags, expected in (({}, "sync"), ({"stream": True}, "stream"),
                                    ({"openai_ws_mode": True}, "ws_v2"),
                                    ({"stream": True, "openai_ws_mode": True}, "ws_v2")):
                with self.subTest(kind=kind, flags=flags):
                    self.assertEqual(project({"request_type": kind, **flags})["request_type"], expected)

    def test_native_request_type_keeps_legacy_projection_for_cyber_and_live(self):
        for kind, expected in ((4, "cyber"), (5, "live")):
            with self.subTest(kind=kind):
                result = project({"request_type": kind, "stream": True})
                self.assertEqual(result["request_type"], "stream")
                self.assertEqual(result["native_request_type"], expected)

        legacy = project({"request_type": 2})
        self.assertEqual(legacy["request_type"], "stream")
        self.assertEqual(legacy["native_request_type"], "stream")

    def test_all_money_is_decimal_text_and_account_cost_has_independent_basis(self):
        money = {"input_cost": Decimal("0.0000000001"), "output_cost": Decimal("0.1000000002"),
                 "cache_creation_cost": Decimal("0.0003"), "cache_read_cost": Decimal("0.0004"),
                 "total_cost": Decimal("0.3000000003"), "actual_cost": Decimal("0.6000000006"),
                 "account_stats_cost": Decimal("0.1000000001"), "account_rate_multiplier": Decimal("1.15"),
                 "rate_multiplier": Decimal("2"), "image_output_cost": Decimal("0.00005"),
                 "image_input_cost": Decimal("0.00006")}
        result = project(money)
        for field, value in money.items():
            self.assertEqual(result[field], str(value), field)
        self.assertEqual(result["account_cost"], "0.115000000115")
        self.assertEqual(json.loads(json.dumps(result))["actual_cost"], "0.6000000006")

    def test_account_cost_preserves_products_beyond_28_significant_digits(self):
        for basis in ("account_stats_cost", "total_cost"):
            with self.subTest(basis=basis):
                result = project({basis: Decimal("0.12345678901234567890123456789"),
                                  "account_rate_multiplier": Decimal("1.2345678901234567890123456789")})
                self.assertEqual(result["account_cost"],
                                 "0.152415787532388367504953515625361987875019051998750190521")

    def test_account_cost_fallback_zero_and_missing_are_distinct(self):
        cases = [({"total_cost": Decimal("0.1"), "account_rate_multiplier": Decimal("0.2")}, Decimal("0.02")),
                 ({"total_cost": Decimal("12.30")}, Decimal("12.30")),
                 ({"account_stats_cost": Decimal("0"), "total_cost": Decimal("9"), "account_rate_multiplier": 5}, Decimal("0")),
                 ({"total_cost": Decimal("9"), "account_rate_multiplier": Decimal("0")}, Decimal("0")),
                 ({"actual_cost": Decimal("7")}, None), ({}, None)]
        for row, expected in cases:
            with self.subTest(row=row):
                value = project(row)["account_cost"]
                self.assertEqual(None if value is None else Decimal(value), expected)

    def test_projection_drops_credentials_headers_bodies_and_unknown_columns(self):
        extra = {name: "PRIVATE_MARKER" for name in ("credentials", "key", "api_key", "password", "token",
                 "access_token", "authorization", "request_headers", "response_headers", "request_body",
                 "response_body", "request", "response", "new_secret_column")}
        result = project({"id": 9, "request_id": "safe-request", **extra})
        self.assertEqual(result["request_id"], "safe-request")
        self.assertFalse(set(extra) & set(result))
        self.assertNotIn("PRIVATE_MARKER", json.dumps(result))

    def test_sparse_history_keeps_unknown_values_null(self):
        result = project({"id": 9})
        for field in ("user_name", "api_key_name", "account_name", "group_name", "input_tokens",
                      "first_token_ms", "duration_ms", "total_cost", "actual_cost", "account_cost",
                      "requested_model", "upstream_model", "upstream_response_model", "created_at"):
            self.assertIsNone(result[field], field)
        self.assertEqual(result["request_type"], "sync")
        self.assertFalse(result["upstream_model_mismatch"])

    def test_timestamps_require_timezone_and_normalize_to_utc(self):
        self.assertEqual(timestamp("2026-09-30T20:00:00+08:00"), NOW)
        self.assertEqual(timestamp("2026-09-30T12:00:00Z"), NOW)
        self.assertEqual(project({"created_at": NOW})["created_at"], NOW.isoformat())
        for value in (None, "", "not-a-date", "2026-09-30", "2026-09-30T12:00:00", datetime(2026, 9, 30)):
            with self.subTest(value=value), self.assertRaises(HTTPException) as raised:
                timestamp(value)
            self.assertEqual(raised.exception.status_code, 422)


class RecordsFixture(unittest.TestCase):
    def setUp(self):
        FixedDateTime.current = NOW
        clock = patch("app.usage_records.datetime", FixedDateTime)
        clock.start()
        self.addCleanup(clock.stop)
        self.db = ReadOnlyFixtureDB()
        self.addCleanup(self.db.close)
        self.service = UsageRecords(self.db)

    def ids(self, **filters):
        return [row["id"] for row in self.service.list(**filters)["items"]]

    def valid_cursor(self):
        self.db.insert(1)
        self.db.insert(2)
        return self.service.list(limit=1)["next_cursor"]


class UsageRecordsTests(RecordsFixture):
    def test_empty_database_and_default_window(self):
        result = self.service.list()
        self.assertEqual(result["items"], [])
        self.assertIsNone(result["next_cursor"])
        self.assertEqual(result["latest_id"], 0)
        params = self.db.statements[-1][1]
        self.assertEqual(params["from_at"].isoformat(), (NOW - timedelta(hours=24)).isoformat())
        self.assertEqual(params["to_at"].isoformat(), NOW.isoformat())
        self.assertEqual(params["limit"], 51)

    def test_window_boundaries_and_descending_time_then_id(self):
        start = NOW - timedelta(hours=1)
        for record_id, at in ((1, start - timedelta(microseconds=1)), (2, start), (3, NOW),
                              (4, NOW), (5, NOW + timedelta(microseconds=1)),
                              (6, NOW - timedelta(minutes=30))):
            self.db.insert(record_id, created_at=at)
        self.assertEqual(self.ids(from_at=start.isoformat(), to_at=NOW.isoformat()), [4, 3, 6, 2])

    def test_account_api_key_and_all_model_stages_can_be_filtered(self):
        fields = ("model", "requested_model", "upstream_model", "upstream_response_model", "model_mapping_chain")
        for record_id, field in enumerate(fields, 1):
            self.db.insert(record_id, **{field: "Contains-NeEdLe-Here"})
        self.db.insert(6, requested_model="needle", account_id=99)
        self.db.insert(7, requested_model="needle", api_key_id=99)
        self.assertEqual(self.ids(account_id=7, api_key_id=4, model="needle"), [5, 4, 3, 2, 1])

    def test_model_wildcards_backslash_and_sql_text_are_literal_parameters(self):
        for record_id, name in enumerate(("prefix%_\\suffix", "prefixXXsuffix", "' OR 1=1 --"), 1):
            self.db.insert(record_id, requested_model=name)
        self.assertEqual(self.ids(model="%_\\"), [1])
        self.assertEqual(self.ids(model="' OR 1=1 --"), [3])
        sql, params = self.db.statements[-1]
        self.assertNotIn("' OR 1=1 --", sql)
        self.assertEqual(params["model"], "%' OR 1=1 --%")

    def test_request_type_filter_matches_legacy_and_explicit_semantics(self):
        fixtures = [(1, 1, True, True), (2, 2, False, True), (3, 3, False, False),
                    (4, None, False, False), (5, 0, True, False), (6, 99, True, True)]
        for record_id, kind, stream, ws in fixtures:
            self.db.insert(record_id, request_type=kind, stream=stream, openai_ws_mode=ws)
        for kind, expected in (("sync", [4, 1]), ("stream", [5, 2]), ("ws_v2", [6, 3])):
            with self.subTest(kind=kind):
                result = self.service.list(request_type=kind)
                self.assertEqual([row["id"] for row in result["items"]], expected)
                self.assertTrue(all(row["request_type"] == kind for row in result["items"]))

    def test_mismatch_filter_agrees_with_projected_flags(self):
        fixtures = [{"requested_model": "alias", "upstream_model": "real", "upstream_response_model": "real"},
                    {"upstream_model": "real", "upstream_response_model": "different"},
                    {"requested_model": "fallback", "upstream_model": "", "upstream_response_model": "different"},
                    {"upstream_response_model": "other"}, {"upstream_model_mismatch": True},
                    {"upstream_response_model": ""}, {"upstream_response_model": None},
                    {"model": None, "upstream_response_model": "unknown-origin"},
                    {"model": "", "upstream_response_model": "unknown-origin"}]
        for record_id, row in enumerate(fixtures, 1):
            self.db.insert(record_id, **row)
        expected = [5, 4, 3, 2]
        self.assertEqual(self.ids(mismatch_only=True), expected)
        self.assertEqual([row["id"] for row in self.service.list()["items"] if row["upstream_model_mismatch"]], expected)

    def test_pagination_has_no_overlap_or_gap_at_identical_timestamps(self):
        for record_id in range(1, 8):
            self.db.insert(record_id)
        seen, cursor = [], None
        for page_number in range(4):
            result = self.service.list(limit=2, cursor=cursor)
            seen.extend(row["id"] for row in result["items"])
            cursor = result["next_cursor"]
            self.assertEqual(bool(cursor), page_number < 3)
            self.assertEqual(result["latest_id"], 7)
        self.assertEqual(seen, [7, 6, 5, 4, 3, 2, 1])

    def test_concurrent_backdated_insert_is_excluded_until_refresh(self):
        for record_id in range(1, 5):
            self.db.insert(record_id)
        first = self.service.list(limit=2)
        with self.assertRaises(HTTPException) as raised:
            self.service.list(cursor=first["next_cursor"], after_id=4)
        self.assertEqual(raised.exception.status_code, 422)
        inserted = []
        def write():
            self.db.insert(5, created_at=NOW - timedelta(hours=1))
            inserted.append(5)
        writer = threading.Thread(target=write)
        writer.start()
        writer.join(timeout=2)
        self.assertFalse(writer.is_alive())
        self.assertEqual(inserted, [5])
        later = self.service.list(limit=2, cursor=first["next_cursor"])
        self.assertEqual([row["id"] for row in later["items"]], [2, 1])
        self.assertIsNone(later["next_cursor"])
        self.assertEqual(later["latest_id"], 4)
        self.assertEqual(self.service.list(after_id=4), {"new_count": 1, "latest_id": 5})
        self.assertEqual(self.ids(), [4, 3, 2, 1, 5])

    def test_cursor_freezes_both_default_window_endpoints(self):
        cursor = self.valid_cursor()
        original = self.db.statements[-1][1]
        FixedDateTime.current = NOW + timedelta(days=2)
        page = self.service.list(limit=1, cursor=cursor)
        self.assertEqual([row["id"] for row in page["items"]], [1])
        later = self.db.statements[-1][1]
        self.assertEqual(later["from_at"], original["from_at"])
        self.assertEqual(later["to_at"], original["to_at"])

    def test_cursor_rejects_each_changed_filter_before_querying(self):
        cursor = self.valid_cursor()
        for changed in ({"from_at": (NOW - timedelta(hours=1)).isoformat()}, {"to_at": NOW.isoformat()},
                        {"account_id": 7}, {"api_key_id": 4}, {"user_id": 3}, {"model": "gpt"},
                        {"request_type": "sync"}, {"mismatch_only": True}):
            with self.subTest(changed=changed):
                before = len(self.db.statements)
                with self.assertRaises(HTTPException) as raised:
                    self.service.list(cursor=cursor, **changed)
                self.assertEqual(raised.exception.status_code, 422)
                self.assertEqual(len(self.db.statements), before)

    def test_bad_cursors_are_422_without_database_access(self):
        valid = json.loads(base64.urlsafe_b64decode(self.valid_cursor()))
        payloads = [{}, [], None, 1, "text"]
        for key in ("signature", "id", "watermark", "at", "from_at", "to_at"):
            payloads.append({k: v for k, v in valid.items() if k != key})
        for key, value in (("id", 0), ("id", "bad"), ("id", float("inf")),
                           ("watermark", -1), ("watermark", float("inf")),
                           ("at", "invalid"), ("from_at", "2026-09-30"), ("to_at", None)):
            payloads.append({**valid, key: value})
        cursors = ["!", "not-base64", "x" * 2049, base64.urlsafe_b64encode(b"\xff").decode()]
        cursors.extend(base64.urlsafe_b64encode(json.dumps(payload).encode()).decode() for payload in payloads)
        for index, cursor in enumerate(cursors):
            with self.subTest(index=index):
                before = len(self.db.statements)
                with self.assertRaises(HTTPException) as raised:
                    self.service.list(cursor=cursor)
                self.assertEqual(raised.exception.status_code, 422)
                self.assertEqual(len(self.db.statements), before)

    def test_invalid_filters_fail_before_database_access(self):
        bad = [{"limit": 0}, {"limit": 101}, {"account_id": 0}, {"api_key_id": -1},
               {"user_id": 0}, {"user_id": -1}, {"after_id": -1},
               {"model": "x" * 201}, {"request_type": "unknown"}, {"from_at": "yesterday"},
               {"to_at": "2026-09-30T12:00:00"}, {"from_at": NOW.isoformat(), "to_at": NOW.isoformat()},
               {"from_at": (NOW + timedelta(seconds=1)).isoformat(), "to_at": NOW.isoformat()}]
        for params in bad:
            with self.subTest(params=params), self.assertRaises(HTTPException) as raised:
                self.service.list(**params)
            self.assertEqual(raised.exception.status_code, 422)
        self.assertEqual(self.db.statements, [])

    def test_new_count_applies_filters_and_is_not_limited_to_page_size(self):
        for record_id in range(1, 6):
            self.db.insert(record_id, request_type=2, upstream_response_model="different")
        self.db.insert(6, account_id=99, request_type=2, upstream_response_model="different")
        self.db.insert(7, api_key_id=99, request_type=2, upstream_response_model="different")
        self.db.insert(8, request_type=1, upstream_response_model="different")
        self.db.insert(9, request_type=2, upstream_response_model="gpt-test")
        self.db.insert(10, request_type=2, upstream_response_model="different", created_at=NOW - timedelta(days=2))
        result = self.service.list(after_id=2, account_id=7, api_key_id=4, model="gpt", request_type="stream",
                                   mismatch_only=True, limit=1)
        self.assertEqual(result, {"new_count": 3, "latest_id": 10})
        self.assertEqual(self.service.list(after_id=10), {"new_count": 0, "latest_id": 10})

    def test_user_filter_is_shared_by_pages_and_new_record_count(self):
        for record_id, owner, key in ((1, 3, 4), (2, 99, 4), (3, 3, 4), (4, 99, 4), (5, 3, 99)):
            self.db.insert(record_id, user_id=owner, api_key_id=key)
        self.db.insert(6, user_id=3, created_at=NOW - timedelta(days=2))
        first = self.service.list(user_id=3, api_key_id=4, account_id=7, limit=1)
        self.assertEqual([item["id"] for item in first["items"]], [3])
        self.db.insert(7, user_id=99)
        self.db.insert(8, user_id=3)
        second = self.service.list(user_id=3, api_key_id=4, account_id=7, limit=1,
                                   cursor=first["next_cursor"])
        self.assertEqual([item["id"] for item in second["items"]], [1])
        self.assertIsNone(second["next_cursor"])
        self.assertEqual(self.service.list(user_id=3, api_key_id=4, after_id=first["latest_id"]),
                         {"new_count": 1, "latest_id": 8})
        self.assertEqual(self.service.list(user_id=99, api_key_id=4, after_id=first["latest_id"]),
                         {"new_count": 1, "latest_id": 8})
        self.assertEqual(self.ids(user_id=3, api_key_id=99), [5])

    def test_user_filter_uses_record_ids_after_directory_rows_are_physically_deleted(self):
        self.db.insert(1, user_id=700, api_key_id=800)
        self.db.insert(2, user_id=701, api_key_id=800)
        result = self.service.list(user_id=700, api_key_id=800)
        self.assertEqual([item["id"] for item in result["items"]], [1])
        self.assertIsNone(result["items"][0]["user_name"])
        self.assertIsNone(result["items"][0]["api_key_name"])
        self.assertEqual(self.service.list(user_id=700, api_key_id=800, after_id=0),
                         {"new_count": 1, "latest_id": 2})

    def test_detail_preserves_history_when_related_rows_are_missing(self):
        self.db.insert(1, user_id=999, api_key_id=999, account_id=999, group_id=999,
                       created_at=NOW - timedelta(days=100))
        self.assertEqual(self.ids(), [])
        result = self.service.detail(1)
        self.assertEqual(result["id"], 1)
        for field in ("user_name", "user_email", "api_key_name", "account_name", "group_name"):
            self.assertIsNone(result[field])

    def test_detail_joins_names_without_dropping_soft_deleted_account(self):
        self.db.insert(1, total_cost=Decimal("0.1234000001"))
        result = self.service.detail(1)
        self.assertEqual((result["user_name"], result["api_key_name"], result["account_name"], result["group_name"]),
                         ("测试用户", "测试 Key", "历史账号", "测试分组"))
        self.assertEqual(result["total_cost"], "0.1234000001")
        self.assertNotIn("PRIVATE_", json.dumps(result))

    def test_detail_returns_404_and_invalid_id_is_422(self):
        with self.assertRaises(HTTPException) as raised:
            self.service.detail(999)
        self.assertEqual(raised.exception.status_code, 404)
        before = len(self.db.statements)
        for record_id in (0, -1):
            with self.assertRaises(HTTPException) as raised:
                self.service.detail(record_id)
            self.assertEqual(raised.exception.status_code, 422)
        self.assertEqual(len(self.db.statements), before)

    def test_list_count_and_detail_use_bounded_read_only_transactions(self):
        self.db.insert(1)
        before = self.db.raw.total_changes
        self.service.list()
        self.service.list(after_id=0)
        self.service.detail(1)
        self.assertEqual(self.db.raw.total_changes, before)
        for index, statements in enumerate(self.db.transactions):
            self.assertIn("READ ONLY", statements[0])
            if index < 2:
                self.assertIn("REPEATABLE READ", statements[0])
            self.assertEqual(statements[1], "SET LOCAL statement_timeout = '5s'")
            for sql in statements[2:]:
                self.assertTrue(sql.lstrip().upper().startswith("SELECT "))
                self.assertNotRegex(sql, r"(?i)\b(?:INSERT|UPDATE|DELETE|ALTER|DROP|TRUNCATE)\b")
                self.assertNotRegex(sql, r"(?i)SELECT\s+(?:\w+\.)?\*")
                self.assertNotRegex(sql, r"(?i)\b(?:credentials|password|request_body|response_body|request_headers|response_headers)\b")
        self.assertIn("LIMIT %(limit)s", self.db.transactions[0][-1])


class UsageRecordDateTests(RecordsFixture):
    def test_single_shanghai_day_includes_both_day_edges_and_excludes_next_midnight(self):
        start = datetime(2026, 9, 29, 16, tzinfo=timezone.utc)
        end = datetime(2026, 9, 30, 16, tzinfo=timezone.utc)
        for record_id, at in enumerate((start - timedelta(microseconds=1), start,
                                       start + timedelta(microseconds=1), end - timedelta(microseconds=1),
                                       end, end + timedelta(microseconds=1)), 1):
            self.db.insert(record_id, created_at=at, actual_cost=Decimal("0.25"))
        result = self.service.list(start_date="2026-09-30", end_date="2026-09-30", include_summary=True)
        self.assertEqual([item["id"] for item in result["items"]], [4, 3, 2])
        self.assertEqual(result["summary"], {"actual_cost": "0.75"})
        self.assertEqual(self.db.statements[-1][1]["from_at"], start)
        self.assertEqual(self.db.statements[-1][1]["to_at"], end)

    def test_leap_day_and_multi_day_range_use_inclusive_calendar_end_date(self):
        start = datetime(2024, 2, 27, 16, tzinfo=timezone.utc)
        leap_start = datetime(2024, 2, 28, 16, tzinfo=timezone.utc)
        march_start = datetime(2024, 2, 29, 16, tzinfo=timezone.utc)
        end = datetime(2024, 3, 1, 16, tzinfo=timezone.utc)
        for record_id, at in enumerate((start - timedelta(microseconds=1), start, leap_start,
                                       march_start - timedelta(microseconds=1), march_start,
                                       end - timedelta(microseconds=1), end), 1):
            self.db.insert(record_id, created_at=at)
        self.assertEqual(self.ids(start_date="2024-02-29", end_date="2024-02-29"), [4, 3])
        self.assertEqual(self.ids(start_date="2024-02-28", end_date="2024-03-01"), [6, 5, 4, 3, 2])

    def test_year_boundary_uses_next_calendar_day_for_end(self):
        self.db.insert(1, created_at=datetime(2025, 12, 30, 16, tzinfo=timezone.utc))
        self.db.insert(2, created_at=datetime(2026, 1, 1, 15, 59, 59, 999999, tzinfo=timezone.utc))
        self.db.insert(3, created_at=datetime(2026, 1, 1, 16, tzinfo=timezone.utc))
        self.assertEqual(self.ids(start_date="2025-12-31", end_date="2026-01-01"), [2, 1])

    def test_invalid_or_mixed_date_filters_fail_before_database_access(self):
        bad = [{"start_date": "2026-09-30"}, {"end_date": "2026-09-30"},
               {"start_date": "", "end_date": ""},
               {"start_date": "2026-10-01", "end_date": "2026-09-30"}]
        for value in ("2026-02-29", "1900-02-29", "2024-02-30", "2026-13-01", "0000-01-01",
                      "2026-9-30", "20260930", "2026/09/30", "2026-W40-3", " 2026-09-30",
                      "2026-09-30 ", "2026-09-30T00:00:00+08:00", "9999-12-31"):
            bad.append({"start_date": value, "end_date": value})
        for field in ("from_at", "to_at"):
            for value in (NOW.isoformat(), ""):
                bad.append({"start_date": "2026-09-30", "end_date": "2026-09-30", field: value})
        for params in bad:
            with self.subTest(params=params), self.assertRaises(HTTPException) as raised:
                self.service.list(**params, include_summary=True)
            self.assertEqual(raised.exception.status_code, 422)
        self.assertEqual(self.db.statements, [])

    def test_date_cursor_keeps_window_and_rejects_changed_or_omitted_dates(self):
        for record_id in range(1, 4):
            self.db.insert(record_id)
        filters = {"start_date": "2026-09-30", "end_date": "2026-09-30"}
        first = self.service.list(**filters, limit=1)
        FixedDateTime.current = NOW + timedelta(days=10)
        next_page = self.service.list(**filters, cursor=first["next_cursor"], limit=1, include_summary=True)
        self.assertEqual([item["id"] for item in next_page["items"]], [2])
        self.assertNotIn("summary", next_page)
        for changed in ({}, {**filters, "start_date": "2026-09-29"},
                        {**filters, "end_date": "2026-10-01"}):
            with self.subTest(changed=changed):
                before = len(self.db.statements)
                with self.assertRaises(HTTPException) as raised:
                    self.service.list(**changed, cursor=first["next_cursor"])
                self.assertEqual(raised.exception.status_code, 422)
                self.assertEqual(len(self.db.statements), before)

    def test_pre_date_api_cursor_payload_still_works_and_freezes_default_window(self):
        for record_id in range(1, 4):
            self.db.insert(record_id)
        for user_id in (None, 3):
            with self.subTest(user_id=user_id):
                criteria = [None, None, None, None, None, None, False]
                if user_id is not None:
                    criteria.append(user_id)
                cursor = base64.urlsafe_b64encode(json.dumps({
                    "at": (NOW - timedelta(minutes=5)).isoformat(), "id": 2, "watermark": 3,
                    "from_at": (NOW - timedelta(hours=24)).isoformat(), "to_at": NOW.isoformat(),
                    "signature": hashlib.sha256(json.dumps(criteria).encode()).hexdigest(),
                }).encode()).decode()
                FixedDateTime.current = NOW + timedelta(days=10)
                result = self.service.list(cursor=cursor, user_id=user_id, include_summary=True)
                self.assertEqual([item["id"] for item in result["items"]], [1])
                self.assertEqual(result["latest_id"], 3)
                self.assertNotIn("summary", result)
                self.assertEqual(self.db.statements[-1][1]["from_at"], NOW - timedelta(hours=24))
                self.assertEqual(self.db.statements[-1][1]["to_at"], NOW)


class UsageRecordSummaryTests(RecordsFixture):
    def aggregate_statements(self):
        return [(sql, params) for sql, params in self.db.statements if re.search(r"\bsum\s*\(", sql, re.I)]

    def test_summary_is_opt_in_and_uses_actual_cost_without_other_cost_bases(self):
        self.db.insert(1, actual_cost=Decimal("0.125"), total_cost=Decimal("99"),
                       account_stats_cost=Decimal("33"), account_rate_multiplier=Decimal("4"))
        self.assertNotIn("summary", self.service.list())
        self.assertNotIn("summary", self.service.list(include_summary=False))
        self.assertEqual(self.aggregate_statements(), [])
        result = self.service.list(include_summary=True)
        self.assertEqual(result["summary"], {"actual_cost": "0.125"})
        self.assertEqual(len(self.aggregate_statements()), 1)

    def test_summary_covers_all_pages_and_is_independent_of_page_limit(self):
        for record_id in range(1, 106):
            self.db.insert(record_id, actual_cost=Decimal("0.1"))
        self.db.insert(106, actual_cost=Decimal("999"), created_at=NOW - timedelta(days=2))
        first = self.service.list(limit=50, include_summary=True)
        self.assertEqual(first["summary"], {"actual_cost": "10.5"})
        self.assertEqual(len(first["items"]), 50)
        seen = [item["id"] for item in first["items"]]
        cursor = first["next_cursor"]
        page_sizes = []
        while cursor:
            page = self.service.list(limit=50, cursor=cursor, include_summary=True)
            page_sizes.append(len(page["items"]))
            self.assertNotIn("summary", page)
            seen.extend(item["id"] for item in page["items"])
            cursor = page["next_cursor"]
        self.assertEqual(page_sizes, [50, 5])
        self.assertEqual(seen, list(reversed(range(1, 106))))
        self.assertEqual(len(self.aggregate_statements()), 1)
        self.assertEqual(self.service.list(limit=1, include_summary=True)["summary"], first["summary"])

    def test_joint_date_user_key_account_model_type_and_mismatch_filters_apply_to_summary(self):
        common = {"model": "billing", "requested_model": "requested", "upstream_model": "forwarded",
                  "upstream_response_model": "returned", "request_type": 2}
        fields = ("model", "requested_model", "upstream_model", "upstream_response_model", "model_mapping_chain")
        for record_id, field in enumerate(fields, 1):
            self.db.insert(record_id, **{**common, field: "Contains-NeEdLe-Here"},
                           actual_cost=Decimal(f"0.{record_id}"))
        noise = ({"user_id": 99}, {"api_key_id": 99}, {"account_id": 99}, {"request_type": 1},
                 {"upstream_model": "same", "upstream_response_model": "same"},
                 {"created_at": datetime(2026, 9, 29, 15, 59, 59, tzinfo=timezone.utc)},
                 {"created_at": datetime(2026, 9, 30, 16, tzinfo=timezone.utc)})
        for record_id, change in enumerate(noise, 6):
            self.db.insert(record_id, **{**common, "model_mapping_chain": "needle", **change},
                           actual_cost=Decimal("100"))
        self.db.insert(13, **common, actual_cost=Decimal("100"))
        filters = {"start_date": "2026-09-30", "end_date": "2026-09-30", "user_id": 3,
                   "api_key_id": 4, "account_id": 7, "model": "needle", "request_type": "stream",
                   "mismatch_only": True}
        result = self.service.list(**filters, include_summary=True, limit=2)
        self.assertEqual(result["summary"], {"actual_cost": "1.5"})
        self.assertEqual([item["id"] for item in result["items"]], [5, 4])
        self.assertEqual(self.service.list(**filters, after_id=0, include_summary=True, limit=1),
                         {"new_count": 5, "latest_id": 13})
        self.assertEqual(len(self.aggregate_statements()), 1)

    def test_summary_retains_history_after_related_entities_are_soft_or_hard_deleted(self):
        self.db.insert_user(10, deleted_at="2026-09-01")
        self.db.insert_key(11, user_id=10, deleted_at="2026-09-01")
        self.db.insert(1, user_id=10, api_key_id=11, actual_cost=Decimal("0.75"))
        filters = {"user_id": 10, "api_key_id": 11, "account_id": 7, "include_summary": True}
        before = self.service.list(**filters)
        self.assertEqual(before["summary"], {"actual_cost": "0.75"})
        self.assertEqual(before["items"][0]["user_name"], "User 10")
        self.assertEqual(self.service.detail(1)["actual_cost"], "0.75")
        for kind, query in (("users", "#10"), ("api_keys", "#11")):
            self.assertEqual(self.service.options(kind=kind, q=query), {"items": [], "next_cursor": None})
        for table in ("users", "api_keys", "accounts", "groups"):
            self.db.raw.execute(f"DELETE FROM {table}")
        after = self.service.list(**filters)
        self.assertEqual(after["summary"], before["summary"])
        self.assertEqual([item["id"] for item in after["items"]], [1])
        for field in ("user_name", "user_email", "api_key_name", "account_name", "group_name"):
            self.assertIsNone(after["items"][0][field])
        self.assertEqual(self.service.detail(1)["actual_cost"], "0.75")
        for kind in ("users", "api_keys"):
            self.assertEqual(self.service.options(kind=kind), {"items": [], "next_cursor": None})

    def test_empty_null_and_zero_cost_totals_are_decimal_strings(self):
        empty = self.service.list(include_summary=True)
        self.assertEqual(empty["summary"], {"actual_cost": "0"})
        self.db.insert(1, actual_cost=None, total_cost=Decimal("50"))
        self.assertEqual(self.service.list(include_summary=True)["summary"], {"actual_cost": "0"})
        self.db.insert(2, actual_cost=Decimal("0.0000"), total_cost=Decimal("50"))
        total = self.service.list(include_summary=True)["summary"]["actual_cost"]
        self.assertIsInstance(total, str)
        self.assertEqual(Decimal(total), Decimal(0))

    def test_summary_preserves_decimal_precision_without_float_or_context_rounding(self):
        cases = [(("0.1", "0.2"), "0.3"),
                 (("123456789012345678901234567890.12345678901234567890123456789",
                   "0.87654321098765432109876543211", "0.00000000000000000000000000001"),
                  "123456789012345678901234567891.00000000000000000000000000001"),
                 (("1E-30", "2E-30"), "0.000000000000000000000000000003")]
        record_id = 0
        for owner, (values, expected) in enumerate(cases, 1):
            for value in values:
                record_id += 1
                self.db.insert(record_id, user_id=owner, actual_cost=Decimal(value))
            with self.subTest(values=values):
                result = self.service.list(user_id=owner, include_summary=True)
                self.assertEqual(result["summary"], {"actual_cost": expected})
                self.assertEqual(json.loads(json.dumps(result))["summary"]["actual_cost"], expected)

    def test_new_count_never_aggregates_even_when_summary_is_requested(self):
        self.db.insert(1, actual_cost=Decimal("1.25"))
        result = self.service.list(after_id=0, include_summary=True)
        self.assertEqual(result, {"new_count": 1, "latest_id": 1})
        self.assertEqual(self.aggregate_statements(), [])

    def test_summary_and_rows_share_read_transaction_and_watermark_during_concurrent_write(self):
        directory = tempfile.TemporaryDirectory(prefix="usage-records-")
        self.addCleanup(directory.cleanup)
        database = Path(directory.name) / "snapshot.sqlite3"
        self.db = ReadOnlyFixtureDB(database)
        self.addCleanup(self.db.close)
        self.db.raw.execute("PRAGMA journal_mode = WAL")
        self.service = UsageRecords(self.db)
        self.db.insert(1, actual_cost=Decimal("0.1"))
        self.db.insert(2, actual_cost=Decimal("0.2"))
        writer = sqlite3.connect(database, isolation_level=None)
        self.addCleanup(writer.close)
        execute = self.db.execute
        committed = []

        def execute_with_concurrent_write(sql, params=None):
            result = execute(sql, params)
            if "max(id)" in sql.lower() and not committed:
                writer.execute("INSERT INTO usage_logs (id,created_at,actual_cost) VALUES (?,?,?)",
                               (3, (NOW - timedelta(hours=1)).isoformat(), "4.0"))
                writer.execute("UPDATE usage_logs SET actual_cost='99.0' WHERE id=2")
                committed.append(True)
            return result

        with patch.object(self.db, "execute", side_effect=execute_with_concurrent_write):
            first = self.service.list(limit=1, include_summary=True)
        self.assertEqual(committed, [True])
        self.assertEqual(first["latest_id"], 2)
        self.assertEqual(first["summary"], {"actual_cost": "0.3"})
        self.assertEqual([(item["id"], item["actual_cost"]) for item in first["items"]], [(2, "0.2")])
        self.assertEqual(len(self.db.transactions), 1)
        statements = self.db.transactions[0]
        self.assertIn("REPEATABLE READ", statements[0])
        self.assertIn("READ ONLY", statements[0])
        self.assertEqual(statements[1], "SET LOCAL statement_timeout = '5s'")
        self.assertTrue(all(sql.lstrip().upper().startswith("SELECT ") for sql in statements[2:]))
        next_page = self.service.list(cursor=first["next_cursor"], limit=1, include_summary=True)
        self.assertEqual([item["id"] for item in next_page["items"]], [1])
        self.assertEqual(next_page["latest_id"], 2)
        self.assertIsNone(next_page["next_cursor"])
        self.assertNotIn("summary", next_page)
        self.assertEqual(self.service.list(after_id=2, include_summary=True), {"new_count": 1, "latest_id": 3})
        self.assertEqual(len(self.aggregate_statements()), 1)
        refreshed = self.service.list(include_summary=True)
        self.assertEqual(refreshed["latest_id"], 3)
        self.assertEqual(refreshed["summary"], {"actual_cost": "103.1"})


class UsageRecordOptionsTests(RecordsFixture):
    USER_FIELDS = {"id", "name", "email", "status", "deleted"}
    KEY_FIELDS = {"id", "name", "user_id", "user_name", "user_email", "status", "deleted"}

    def setUp(self):
        super().setUp()
        self.db.raw.execute("DELETE FROM api_keys")
        self.db.raw.execute("DELETE FROM users")

    def seed_options(self):
        self.db.insert_user(10, username="同名", email="first@example.invalid")
        self.db.insert_user(20, username="同名", email="second@example.invalid", status="disabled")
        self.db.insert_user(30, username="已删除用户", deleted_at="2026-01-01T00:00:00+00:00")
        self.db.insert_user(40)
        self.db.insert_key(10, name="同名 Key", user_id=10)
        self.db.insert_key(20, name="同名 Key", user_id=20, status="disabled")
        self.db.insert_key(40, name="已删除 Key", user_id=10,
                           deleted_at="2026-01-01T00:00:00+00:00")
        self.db.insert_key(50, name="软删除用户的 Key", user_id=30)
        self.db.insert_key(60, name="物理删除用户的 Key", user_id=40)
        self.db.insert_key(70, name="不存在用户的 Key", user_id=999)
        self.db.raw.execute("DELETE FROM users WHERE id = 40")

    def test_more_than_100_users_and_keys_are_complete_in_ascending_id_pages(self):
        for record_id in reversed(range(1, 181)):
            self.db.insert_user(record_id, username="同名候选",
                                deleted_at="2026-01-01" if record_id % 6 == 0 else None)
            self.db.insert_key(record_id, name="同名候选", user_id=record_id,
                               deleted_at="2026-01-01" if record_id % 7 == 0 else None)
        self.assertEqual(self.db.raw.execute("SELECT count(*) FROM usage_logs").fetchone()[0], 0)
        for kind in ("users", "api_keys"):
            with self.subTest(kind=kind):
                expected = [record_id for record_id in range(1, 181)
                            if record_id % 6 and (kind == "users" or record_id % 7)]
                seen, cursor = [], None
                for offset in range(0, len(expected), 50):
                    page = self.service.options(kind=kind, cursor=cursor)
                    self.assertEqual([item["id"] for item in page["items"]], expected[offset:offset + 50])
                    seen.extend(item["id"] for item in page["items"])
                    cursor = page["next_cursor"]
                    self.assertEqual(bool(cursor), offset + 50 < len(expected))
                self.assertEqual(seen, expected)
                self.assertEqual(len(self.service.options(kind=kind, limit=100)["items"]), 100)

    def test_users_directory_excludes_deleted_but_retains_duplicate_names_and_disabled_entries(self):
        self.seed_options()
        result = self.service.options(kind="users")
        self.assertEqual(result, {"items": [
            {"id": 10, "name": "同名", "email": "first@example.invalid", "status": "active", "deleted": False},
            {"id": 20, "name": "同名", "email": "second@example.invalid", "status": "disabled", "deleted": False}],
            "next_cursor": None})
        self.assertTrue(all(type(item["deleted"]) is bool for item in result["items"]))

    def test_key_directory_excludes_deleted_and_orphaned_keys_but_retains_disabled_owner_identity(self):
        self.seed_options()
        result = self.service.options(kind="api_keys")
        self.assertEqual([item["id"] for item in result["items"]], [10, 20])
        self.assertEqual(result["items"][1], {"id": 20, "name": "同名 Key", "user_id": 20,
                         "user_name": "同名", "user_email": "second@example.invalid", "status": "disabled", "deleted": False})
        self.assertTrue(all(type(item["deleted"]) is bool for item in result["items"]))

    def test_deleted_entities_cannot_bypass_directory_filter_by_id_name_or_owner(self):
        self.seed_options()
        cases = (("users", 30, "已删除用户", None), ("users", 40, "User 40", None),
                 ("api_keys", 40, "已删除 Key", 10), ("api_keys", 50, "软删除用户的 Key", 30),
                 ("api_keys", 60, "物理删除用户的 Key", 40), ("api_keys", 70, "不存在用户的 Key", 999))
        for kind, record_id, name, owner in cases:
            for query in (str(record_id), f"#{record_id}", name):
                for user_id in {None, owner}:
                    with self.subTest(kind=kind, query=query, owner=user_id):
                        self.assertEqual(self.service.options(kind=kind, q=query, user_id=user_id),
                                         {"items": [], "next_cursor": None})

    def test_user_search_matches_name_email_or_id(self):
        self.db.insert_user(101, username="ALIce", email="first@example.invalid")
        self.db.insert_user(802, username="另一用户", email="unique-address@example.invalid")
        self.db.insert_user(1802, username="编号不同", email="unrelated@example.invalid")
        for query, expected in (("alice", [101]), ("unique-address", [802]), ("802", [802]),
                                ("#802", [802]), ("missing", [])):
            with self.subTest(query=query):
                self.assertEqual([item["id"] for item in self.service.options(kind="users", q=query)["items"]], expected)

    def test_key_search_by_name_or_id_is_combined_with_user_scope(self):
        self.db.insert_user(10)
        self.db.insert_user(20)
        self.db.insert_key(101, name="Shared Key", user_id=10)
        self.db.insert_key(504, name="Shared Key", user_id=20)
        self.db.insert_key(605, name="Other Key", user_id=20)
        self.db.insert_key(1504, name="Unrelated", user_id=10)
        for query, owner, expected in (("shared", None, [101, 504]), ("shared", 20, [504]),
                                        ("504", None, [504]), ("#504", None, [504]),
                                        ("504", 10, []), (None, 20, [504, 605])):
            with self.subTest(query=query, owner=owner):
                rows = self.service.options(kind="api_keys", q=query, user_id=owner)["items"]
                self.assertEqual([item["id"] for item in rows], expected)

    def test_directory_search_escapes_wildcards_and_parameterizes_sql(self):
        for record_id, name in enumerate(("prefix%_\\suffix", "prefixXXsuffix", "' OR 1=1 --"), 1):
            self.db.insert_user(record_id, username=name)
            self.db.insert_key(record_id, name=name)
        for kind in ("users", "api_keys"):
            for query, expected in (("%_\\", [1]), ("' OR 1=1 --", [3])):
                with self.subTest(kind=kind, query=query):
                    result = self.service.options(kind=kind, q=query)
                    self.assertEqual([item["id"] for item in result["items"]], expected)
                    self.assertNotIn(query, self.db.statements[-1][0])

    def test_directory_output_is_an_exact_whitelist_and_never_returns_secrets(self):
        self.seed_options()
        for kind, fields in (("users", self.USER_FIELDS), ("api_keys", self.KEY_FIELDS)):
            with self.subTest(kind=kind):
                result = self.service.options(kind=kind)
                self.assertEqual(set(result), {"items", "next_cursor"})
                for item in result["items"]:
                    self.assertEqual(set(item), fields)
                self.assertNotIn("PRIVATE_", json.dumps(result))

    def test_empty_directory_or_unknown_owner_returns_no_cursor(self):
        for kind in ("users", "api_keys"):
            self.assertEqual(self.service.options(kind=kind), {"items": [], "next_cursor": None})
        self.seed_options()
        self.assertEqual(self.service.options(kind="api_keys", user_id=999), {"items": [], "next_cursor": None})

    def test_option_cursor_is_bound_to_kind_query_and_user_scope(self):
        self.seed_options()
        user_cursor = self.service.options(kind="users", q="同名", limit=1)["next_cursor"]
        self.db.insert_key(30, name="同名 Key", user_id=10)
        key_cursor = self.service.options(kind="api_keys", q="同名", user_id=10, limit=1)["next_cursor"]
        changes = [(user_cursor, {"kind": "api_keys", "q": "同名"}),
                   (user_cursor, {"kind": "users", "q": "another"}),
                   (key_cursor, {"kind": "api_keys", "q": "同名", "user_id": 20}),
                   (key_cursor, {"kind": "api_keys", "q": "同名"})]
        for cursor, params in changes:
            with self.subTest(params=params):
                before = len(self.db.statements)
                with self.assertRaises(HTTPException) as raised:
                    self.service.options(cursor=cursor, **params)
                self.assertEqual(raised.exception.status_code, 422)
                self.assertEqual(len(self.db.statements), before)

    def test_invalid_option_parameters_and_cursors_do_not_reach_database(self):
        bad = [{"kind": "accounts"}, {"limit": 0}, {"limit": 101},
               {"kind": "api_keys", "user_id": 0}, {"kind": "api_keys", "user_id": -1}]
        bad.extend({"cursor": cursor} for cursor in ("!", "not-base64", "x" * 4097,
                    base64.urlsafe_b64encode(b"null").decode(),
                    base64.urlsafe_b64encode(b'{"id":Infinity}').decode()))
        for params in bad:
            with self.subTest(params=params), self.assertRaises(HTTPException) as raised:
                    self.service.options(**{"kind": "users", **params})
            self.assertEqual(raised.exception.status_code, 422)
        self.assertEqual(self.db.statements, [])

    def test_options_use_bounded_read_only_directory_queries_without_usage_history(self):
        self.seed_options()
        before = self.db.raw.total_changes
        for kind in ("users", "api_keys"):
            self.service.options(kind=kind, q="同名")
        self.assertEqual(self.db.raw.total_changes, before)
        for statements in self.db.transactions:
            self.assertIn("READ ONLY", statements[0])
            self.assertEqual(statements[1], "SET LOCAL statement_timeout = '5s'")
            for sql in statements[2:]:
                self.assertTrue(sql.lstrip().upper().startswith("SELECT "))
                self.assertNotIn("usage_logs", sql)
                self.assertNotRegex(sql, r"(?i)\b(?:INSERT|UPDATE|DELETE|ALTER|DROP|TRUNCATE)\b")
                self.assertNotRegex(sql, r"(?i)SELECT\s+(?:\w+\.)?\*")
                self.assertNotRegex(sql, r"(?i)\b(?:credentials|password|key|token)\b")
            self.assertIn("LIMIT %(limit)s", statements[-1])


class UsageRecordsRouteTests(RecordsFixture):
    def setUp(self):
        super().setUp()
        self.db.insert(1, requested_model="alias", upstream_model="upstream", upstream_response_model="returned",
                       request_type=2, total_cost=Decimal("0.0000000001"))
        app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)
        self.runtime = SimpleNamespace(db=self.db, oauth_monitor=Mock(), bark_notifier=Mock(), oauth_base_url=Mock())
        service = install_desktop_api(app, self.runtime)
        self.desktop_service = service
        self.addCleanup(lambda: asyncio.run(service.close()))
        def authenticate(key, *, fresh=False):
            if key != "fixture-admin-key":
                raise HTTPException(401, "invalid admin key")
        self.auth = Mock(side_effect=authenticate)
        service.authenticate = self.auth
        self.client = TestClient(app)
        self.addCleanup(self.client.close)
        self.headers = {"x-api-key": "fixture-admin-key"}

    def test_list_and_detail_require_admin_header_before_any_database_read(self):
        for path in (PATH, PATH + "/1", OPTIONS_PATH):
            for kwargs in ({}, {"headers": {"x-api-key": "bad"}},
                           {"headers": {"Authorization": "Bearer fixture-admin-key"}},
                           {"params": {"x-api-key": "fixture-admin-key"}}):
                with self.subTest(path=path, kwargs=kwargs):
                    if path == OPTIONS_PATH:
                        kwargs = {**kwargs, "params": {"kind": "users", **kwargs.get("params", {})}}
                    self.assertEqual(self.client.get(path, **kwargs).status_code, 401)
        self.assertEqual(self.db.statements, [])

    def test_authenticated_list_and_detail_are_safe_serializable_read_responses(self):
        for path in (PATH, PATH + "/1"):
            with self.subTest(path=path):
                response = self.client.get(path, headers=self.headers)
                self.assertEqual(response.status_code, 200, response.text)
                result = response.json()
                record = result["items"][0] if path == PATH else result
                self.assertEqual(record["requested_model"], "alias")
                self.assertEqual(record["upstream_model"], "upstream")
                self.assertEqual(record["upstream_response_model"], "returned")
                self.assertEqual(Decimal(record["total_cost"]), Decimal("0.0000000001"))
                self.assertNotIn("PRIVATE_", response.text)
                self.assertEqual(response.headers["cache-control"], "no-store")
                self.assertTrue(response.headers["x-request-id"])
        self.assertEqual(self.auth.call_count, 2)
        self.auth.assert_called_with("fixture-admin-key", fresh=False)

    def test_route_filters_and_new_count_are_applied(self):
        self.db.insert(2, request_type=1)
        response = self.client.get(PATH, headers=self.headers, params={"account_id": 7, "api_key_id": 4,
            "request_type": "stream", "model": "alias", "mismatch_only": True})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual([item["id"] for item in response.json()["items"]], [1])
        response = self.client.get(PATH, headers=self.headers, params={"after_id": 1})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json(), {"new_count": 1, "latest_id": 2})

    def test_records_route_passes_user_id_to_list_and_new_count(self):
        self.db.insert(2, user_id=900, api_key_id=901)
        response = self.client.get(PATH, headers=self.headers, params={"user_id": 900, "api_key_id": 901})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual([item["id"] for item in response.json()["items"]], [2])
        self.assertIsNone(response.json()["items"][0]["user_name"])
        for owner, expected in ((3, 0), (900, 1)):
            response = self.client.get(PATH, headers=self.headers, params={"user_id": owner, "after_id": 1})
            self.assertEqual(response.status_code, 200, response.text)
            self.assertEqual(response.json()["new_count"], expected)

    def test_route_passes_date_window_and_summary_with_exact_decimal_json(self):
        self.db.insert(2, created_at=datetime(2026, 9, 29, 16, tzinfo=timezone.utc), actual_cost=Decimal("0.1"))
        self.db.insert(3, created_at=datetime(2026, 9, 30, 15, 59, 59, 999999, tzinfo=timezone.utc),
                       actual_cost=Decimal("0.2"))
        self.db.insert(4, created_at=datetime(2026, 9, 30, 16, tzinfo=timezone.utc), actual_cost=Decimal("999"))
        params = {"start_date": "2026-09-30", "end_date": "2026-09-30", "include_summary": "true", "limit": 1}
        response = self.client.get(PATH, headers=self.headers, params=params)
        self.assertEqual(response.status_code, 200, response.text)
        first = response.json()
        self.assertEqual([item["id"] for item in first["items"]], [3])
        self.assertEqual(first["summary"], {"actual_cost": "0.3"})
        self.assertEqual(response.headers["cache-control"], "no-store")
        self.assertTrue(response.headers["x-request-id"])
        response = self.client.get(PATH, headers=self.headers, params={**params, "cursor": first["next_cursor"]})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual([item["id"] for item in response.json()["items"]], [1])
        self.assertNotIn("summary", response.json())
        response = self.client.get(PATH, headers=self.headers, params={**params, "after_id": 1})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json(), {"new_count": 2, "latest_id": 4})
        self.assertEqual(sum(bool(re.search(r"\bsum\s*\(", sql, re.I)) for sql, _ in self.db.statements), 1)

    def test_route_summary_default_and_explicit_false_do_not_aggregate(self):
        for params in ({}, {"include_summary": "false"}):
            with self.subTest(params=params):
                response = self.client.get(PATH, headers=self.headers, params=params)
                self.assertEqual(response.status_code, 200, response.text)
                self.assertNotIn("summary", response.json())
        self.assertFalse(any(re.search(r"\bsum\s*\(", sql, re.I) for sql, _ in self.db.statements))

    def test_route_rejects_invalid_dates_mixed_time_modes_and_summary_boolean(self):
        bad = [{"include_summary": "maybe"}, {"start_date": "2026-09-30"}, {"end_date": "2026-09-30"},
               {"start_date": "2026-02-29", "end_date": "2026-02-29"},
               {"start_date": "2026-9-30", "end_date": "2026-09-30"},
               {"start_date": "2026-10-01", "end_date": "2026-09-30"},
               {"start_date": "9999-12-31", "end_date": "9999-12-31"},
               {"start_date": "2026-09-30", "end_date": "2026-09-30", "from_at": NOW.isoformat()},
               {"start_date": "2026-09-30", "end_date": "2026-09-30", "to_at": ""}]
        for params in bad:
            with self.subTest(params=params):
                response = self.client.get(PATH, headers=self.headers, params=params)
                self.assertEqual(response.status_code, 422, response.text)
        self.assertEqual(self.db.statements, [])

    def test_summary_reads_never_trigger_quota_models_oauth_or_http(self):
        self.db.insert(2, actual_cost=Decimal("1.25"))
        before = self.db.raw.total_changes
        params = {"start_date": "2026-09-30", "end_date": "2026-09-30", "include_summary": True, "limit": 1}
        with patch.object(self.desktop_service, "usage_action") as usage_action, \
             patch.object(self.desktop_service.actions, "models") as models, \
             patch.object(self.desktop_service.actions, "start_batch") as quota_refresh, \
             patch.object(self.desktop_service.actions, "prepare_test") as prepare_test, \
             patch("app.model_catalog.ModelCatalogService.source_catalog") as source_catalog, \
             patch("urllib.request.OpenerDirector.open") as urllib_send, \
             patch("httpx.HTTPTransport.handle_request") as http_send, \
             patch("httpx.AsyncHTTPTransport.handle_async_request") as async_http_send:
            first = self.client.get(PATH, headers=self.headers, params=params)
            self.assertEqual(first.status_code, 200, first.text)
            self.assertEqual(first.json()["summary"], {"actual_cost": "1.25"})
            for extra in ({"cursor": first.json()["next_cursor"]}, {"after_id": 1}, {}):
                response = self.client.get(PATH, headers=self.headers, params={**params, **extra})
                self.assertEqual(response.status_code, 200, response.text)
            for action in (usage_action, models, quota_refresh, prepare_test, source_catalog,
                           urllib_send, http_send, async_http_send):
                action.assert_not_called()
        self.assertEqual(self.db.raw.total_changes, before)
        self.assertEqual(self.runtime.oauth_monitor.mock_calls, [])
        self.assertEqual(self.runtime.bark_notifier.mock_calls, [])
        self.runtime.oauth_base_url.assert_not_called()
        self.auth.assert_called_with("fixture-admin-key", fresh=False)

    def test_options_route_filters_returns_whitelist_and_never_runs_oauth_or_http(self):
        self.db.insert_user(9, username="测试用户", email="other@example.invalid")
        self.db.insert_key(5, name="测试 Key", user_id=9)
        with patch("urllib.request.OpenerDirector.open") as urllib_send, \
             patch("httpx.HTTPTransport.handle_request") as http_send, \
             patch("httpx.AsyncHTTPTransport.handle_async_request") as async_http_send:
            users = self.client.get(OPTIONS_PATH, headers=self.headers, params={"kind": "users", "q": "test@"})
            keys = self.client.get(OPTIONS_PATH, headers=self.headers,
                                   params={"kind": "api_keys", "q": "测试 Key", "user_id": 3})
        for response in (users, keys):
            self.assertEqual(response.status_code, 200, response.text)
            self.assertEqual(response.headers["cache-control"], "no-store")
            self.assertTrue(response.headers["x-request-id"])
            self.assertNotIn("PRIVATE_", response.text)
        self.assertEqual([item["id"] for item in users.json()["items"]], [3])
        self.assertEqual([item["id"] for item in keys.json()["items"]], [4])
        self.assertEqual(set(users.json()["items"][0]), UsageRecordOptionsTests.USER_FIELDS)
        self.assertEqual(set(keys.json()["items"][0]), UsageRecordOptionsTests.KEY_FIELDS)
        self.assertFalse(any("usage_logs" in sql for sql, _ in self.db.statements))
        self.assertEqual(self.runtime.oauth_monitor.mock_calls, [])
        self.assertEqual(self.runtime.bark_notifier.mock_calls, [])
        self.runtime.oauth_base_url.assert_not_called()
        urllib_send.assert_not_called()
        http_send.assert_not_called()
        async_http_send.assert_not_called()
        self.auth.assert_called_with("fixture-admin-key", fresh=False)

    def test_options_route_rejects_invalid_parameters_and_cross_condition_cursor(self):
        for params in ({"kind": "accounts"}, {"limit": 101}, {"limit": "bad"},
                       {"kind": "api_keys", "user_id": 0}, {"user_id": "not-an-id"}, {"cursor": "!"}):
            with self.subTest(params=params):
                response = self.client.get(OPTIONS_PATH, headers=self.headers, params={"kind": "users", **params})
                self.assertEqual(response.status_code, 422, response.text)
        self.assertEqual(self.db.statements, [])
        self.db.insert_user(9, username="测试用户")
        first = self.client.get(OPTIONS_PATH, headers=self.headers, params={"kind": "users", "q": "测试", "limit": 1})
        self.assertEqual(first.status_code, 200, first.text)
        response = self.client.get(OPTIONS_PATH, headers=self.headers,
                                   params={"kind": "api_keys", "q": "测试", "cursor": first.json()["next_cursor"]})
        self.assertEqual(response.status_code, 422, response.text)

    def test_invalid_route_parameters_are_422_and_absent_detail_is_404(self):
        for suffix, params in (("", {"limit": 0}), ("", {"limit": "not-an-integer"}),
                               ("", {"account_id": -1}), ("", {"user_id": 0}), ("", {"user_id": "bad"}),
                               ("", {"mismatch_only": "maybe"}),
                               ("", {"cursor": "!"}), ("/0", {}), ("/bad", {})):
            with self.subTest(suffix=suffix, params=params):
                response = self.client.get(PATH + suffix, params=params, headers=self.headers)
                self.assertEqual(response.status_code, 422, response.text)
        self.assertEqual(self.db.statements, [])
        self.assertEqual(self.client.get(PATH + "/999", headers=self.headers).status_code, 404)

    def test_malformed_cursor_numbers_return_422_at_http_boundary(self):
        self.db.insert(2)
        response = self.client.get(PATH, headers=self.headers, params={"limit": 1})
        cursor = json.loads(base64.urlsafe_b64decode(response.json()["next_cursor"]))
        for field in ("id", "watermark"):
            with self.subTest(field=field):
                value = base64.urlsafe_b64encode(json.dumps({**cursor, field: float("inf")}).encode()).decode()
                before = len(self.db.statements)
                response = self.client.get(PATH, headers=self.headers, params={"cursor": value})
                self.assertEqual(response.status_code, 422, response.text)
                self.assertEqual(len(self.db.statements), before)

    def test_usage_records_expose_no_write_method(self):
        for method in ("POST", "PUT", "PATCH", "DELETE"):
            for path in (PATH, PATH + "/1", OPTIONS_PATH):
                with self.subTest(method=method, path=path):
                    self.assertEqual(self.client.request(method, path, headers=self.headers).status_code, 405)
        self.assertEqual(self.db.statements, [])


if __name__ == "__main__":
    unittest.main()
