from __future__ import annotations

import asyncio
import base64
import json
import re
import sqlite3
import threading
import unittest
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from types import SimpleNamespace
from unittest.mock import Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from app.desktop_api import install_desktop_api
from app.usage_records import UsageRecords, project, timestamp


NOW = datetime(2026, 9, 30, 12, tzinfo=timezone.utc)
PATH = "/api/desktop/v1/usage-records"


class FixedDateTime(datetime):
    current = NOW

    @classmethod
    def now(cls, tz=None):
        return cls.fromisoformat(cls.current.astimezone(tz or timezone.utc).isoformat())


class ReadOnlyFixtureDB:
    """Execute SELECT semantics in SQLite; check the PostgreSQL transaction contract.

    Only named parameters and ILIKE need translation. This is deliberately not a
    Python reimplementation of filtering or pagination, nor PostgreSQL acceptance.
    """

    def __init__(self):
        self.raw = sqlite3.connect(":memory:", check_same_thread=False, isolation_level=None)
        self.raw.row_factory = sqlite3.Row
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
            CREATE TABLE users (id INTEGER, username TEXT, email TEXT, password TEXT);
            CREATE TABLE api_keys (id INTEGER, name TEXT, key TEXT);
            CREATE TABLE accounts (id INTEGER, name TEXT, credentials TEXT, deleted_at TEXT);
            CREATE TABLE groups (id INTEGER, name TEXT);
            INSERT INTO users VALUES (3, '测试用户', 'test@example.invalid', 'PRIVATE_PASSWORD');
            INSERT INTO api_keys VALUES (4, '测试 Key', 'PRIVATE_API_KEY');
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
                        {"account_id": 7}, {"api_key_id": 4}, {"model": "gpt"},
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
        bad = [{"limit": 0}, {"limit": 101}, {"account_id": 0}, {"api_key_id": -1}, {"after_id": -1},
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


class UsageRecordsRouteTests(RecordsFixture):
    def setUp(self):
        super().setUp()
        self.db.insert(1, requested_model="alias", upstream_model="upstream", upstream_response_model="returned",
                       request_type=2, total_cost=Decimal("0.0000000001"))
        app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)
        service = install_desktop_api(app, SimpleNamespace(db=self.db))
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
        for path in (PATH, PATH + "/1"):
            for kwargs in ({}, {"headers": {"x-api-key": "bad"}},
                           {"headers": {"Authorization": "Bearer fixture-admin-key"}},
                           {"params": {"x-api-key": "fixture-admin-key"}}):
                with self.subTest(path=path, kwargs=kwargs):
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

    def test_invalid_route_parameters_are_422_and_absent_detail_is_404(self):
        for suffix, params in (("", {"limit": 0}), ("", {"limit": "not-an-integer"}),
                               ("", {"account_id": -1}), ("", {"mismatch_only": "maybe"}),
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
            for path in (PATH, PATH + "/1"):
                with self.subTest(method=method, path=path):
                    self.assertEqual(self.client.request(method, path, headers=self.headers).status_code, 405)
        self.assertEqual(self.db.statements, [])


if __name__ == "__main__":
    unittest.main()
