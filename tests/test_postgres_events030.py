from __future__ import annotations

import json
import os
import unittest
import uuid
from contextlib import contextmanager
from types import SimpleNamespace
from urllib.parse import unquote, urlsplit

from fastapi import HTTPException

from app.desktop_api import DesktopService
from app.error_evidence import ERROR_WHERE, MESSAGES, error_category_sql


@contextmanager
def _postgres_fixture():
    dsn = os.environ.get("SUB2OPS_TEST_DATABASE_URL", "")
    if not dsn:
        raise unittest.SkipTest("SUB2OPS_TEST_DATABASE_URL is not set")
    parsed = urlsplit(dsn)
    if (parsed.scheme not in {"postgres", "postgresql"}
            or parsed.hostname not in {"localhost", "127.0.0.1"}
            or unquote(parsed.path) != "/sub2ops_qa" or parsed.query or parsed.fragment):
        raise ValueError("PostgreSQL tests require a loopback URL for sub2ops_qa without query overrides")

    import psycopg
    from psycopg import sql
    from psycopg.rows import dict_row

    # Explicit hostaddr also prevents PGHOSTADDR from redirecting a localhost URL.
    connection = psycopg.connect(dsn, host="127.0.0.1", hostaddr="127.0.0.1", dbname="sub2ops_qa",
                                  options="", connect_timeout=5, autocommit=True, row_factory=dict_row)
    schema = "sub2ops_events030_" + uuid.uuid4().hex
    created = False
    try:
        connection.execute(sql.SQL("CREATE SCHEMA {}").format(sql.Identifier(schema)))
        created = True
        connection.execute(sql.SQL("SET search_path TO {}, pg_catalog").format(sql.Identifier(schema)))
        connection.execute("""CREATE TABLE accounts (
            id bigint PRIMARY KEY, name text, platform text, type text, deleted_at timestamptz
        )""")
        connection.execute("""CREATE TABLE groups (
            id bigint PRIMARY KEY, name text, deleted_at timestamptz
        )""")
        connection.execute("""CREATE TABLE ops_error_logs (
            id bigint PRIMARY KEY, account_id bigint, group_id bigint,
            created_at timestamptz DEFAULT '2026-10-08T03:00:00+00:00',
            platform text, model text, requested_model text, upstream_model text,
            status_code integer, upstream_status_code integer, provider_error_code text,
            error_type text, error_message text, upstream_error_message text,
            error_owner text, error_phase text, error_source text, stream boolean,
            error_body text, upstream_error_detail text, upstream_errors jsonb,
            request_id text, resolved boolean DEFAULT false
        )""")
        yield connection
    finally:
        try:
            if created:
                connection.execute("SET search_path TO pg_catalog")
                connection.execute(sql.SQL("DROP SCHEMA {} CASCADE").format(sql.Identifier(schema)))
        finally:
            connection.close()


class _PostgresRows:
    def __init__(self, connection):
        self.connection = connection

    def fetch_all(self, query, params=None):
        return self.connection.execute(query, params).fetchall()

    def fetch_one(self, query, params=None):
        return self.connection.execute(query, params).fetchone()


class PostgreSQLEvents030Tests(unittest.TestCase):
    def setUp(self):
        self.connection = self.enterContext(_postgres_fixture())
        self.service = DesktopService(SimpleNamespace(db=_PostgresRows(self.connection)))
        self.rows, self.names = {}, {}
        self.expected = {"degradation": set(), "other": set(), None: set()}
        self.excluded = set()
        self.connection.execute("""INSERT INTO accounts (id,name,platform,type,deleted_at) VALUES
            (1,'fixture-oauth','openai','oauth',NULL),
            (2,'fixture-key','openai','apikey',NULL),
            (3,'fixture-platform','grok','oauth',NULL),
            (4,'fixture-auth-type','openai','other',NULL),
            (5,'fixture-deleted','openai','oauth','2026-10-01T00:00:00+00:00')""")
        self.connection.execute("""INSERT INTO groups (id,name,deleted_at) VALUES
            (1,'fixture-group',NULL),
            (2,'fixture-deleted-group','2026-10-01T00:00:00+00:00')""")
        self._populate()

    def _add(self, name, category, *, legacy=True, **changes):
        from psycopg import sql
        from psycopg.types.json import Jsonb

        row = {
            "id": len(self.rows) + 1, "account_id": 1, "group_id": 1,
            "platform": "openai", "model": "fixture-model", "requested_model": "fixture-model",
            "upstream_model": "fixture-model", "status_code": 503, "upstream_status_code": 503,
            "error_owner": "provider", "error_phase": "upstream", "error_source": "upstream",
            "stream": True, "upstream_error_message": MESSAGES[0],
            "error_message": None, "error_body": None, "upstream_error_detail": None,
            "upstream_errors": None, "request_id": "fixture-request", **changes,
        }
        query = sql.SQL("INSERT INTO ops_error_logs ({}) VALUES ({})").format(
            sql.SQL(",").join(map(sql.Identifier, row)),
            sql.SQL(",").join(sql.Placeholder() for _ in row),
        )
        values = [Jsonb(value) if key == "upstream_errors" and value is not None else value
                  for key, value in row.items()]
        self.connection.execute(query, values)
        self.rows[row["id"]], self.names[name] = row, row["id"]
        if category is None:
            self.excluded.add(row["id"])
        else:
            self.expected[category].add(row["id"])
        if legacy:
            self.expected[None].add(row["id"])

    def _populate(self):
        for aid, kind in ((1, "oauth"), (2, "key")):
            for index, message in enumerate(MESSAGES):
                self._add(f"{kind}-capacity-{index}", "degradation", account_id=aid,
                          upstream_error_message=message)
        self._add("normalized-capacity", "degradation",
                  upstream_error_message="  " + MESSAGES[1].upper().replace(" ", "  ") + "  ")
        self._add("direct-stream-capacity", "degradation", upstream_error_message=None,
                  error_message=MESSAGES[2])
        self._add("body-capacity", "degradation", upstream_error_message=None,
                  error_body=json.dumps({"error": {"message": MESSAGES[1]}}))
        self._add("detail-capacity", "degradation", upstream_error_message=None,
                  upstream_error_detail=json.dumps({"error": {"message": MESSAGES[0]}}))
        self._add("array-capacity", "degradation", upstream_error_message=None,
                  upstream_errors=[{"error": {"message": MESSAGES[2]}}])
        self._add("mixed-array-capacity", "degradation", upstream_error_message=None,
                  upstream_errors=[{"message": "您已达到请求数限制：每分钟最多请求 15 次"},
                                   {"message": MESSAGES[2]}])
        for status in ("failed", "incomplete"):
            self._add(f"gateway-{status}", "degradation", account_id=2, stream=False,
                      error_owner="platform", error_phase="internal", error_source="gateway",
                      upstream_error_message=None,
                      error_body=json.dumps({"type": f"response.{status}",
                                             "response": {"error": {"message": MESSAGES[0]}}}))
        self._add("gateway-stream", "degradation", error_owner="platform", error_phase="internal",
                  error_source="gateway", upstream_error_message=None, error_message=MESSAGES[2])
        self._add("upstream-capacity-429", "degradation", upstream_status_code=429, status_code=429)
        self._add("soft-deleted-capacity", "degradation", account_id=5, group_id=2)

        self._add("orphan", "other", legacy=False, account_id=None, group_id=None)
        self._add("missing-entities", "other", account_id=99, group_id=99)
        self._add("soft-deleted-other", "other", account_id=5, group_id=2,
                  upstream_error_message="fixture upstream failure")
        self._add("other-platform", "other", account_id=3)
        self._add("other-auth-type", "other", account_id=4)
        self._add("account-auth", "other", error_phase="account_auth", status_code=401,
                  upstream_status_code=401)
        self._add("platform-internal", "other", legacy=False, error_owner="platform",
                  error_phase="internal", error_source="platform")
        self._add("client-error", "other", legacy=False, error_owner="client", error_phase="request")
        self._add("real-upstream-429", "other", upstream_status_code=429, status_code=429,
                  upstream_error_message="Rate limit exceeded")
        self._add("other-stream", "other", upstream_error_message="stream disconnected before completion")
        self._add("partial-capacity", "other", upstream_error_message="Selected model is at capacity.")
        self._add("upstream-concurrency", "other", upstream_status_code=429, status_code=429,
                  upstream_error_message="Concurrency limit exceeded for account, please retry later")
        self._add("request-capacity", "other", upstream_error_message=None,
                  error_body=json.dumps({"request": {"error": {"message": MESSAGES[0]}}}))
        self._add("request-local", "other", upstream_error_message=None,
                  error_body=json.dumps({"request": {"error": {
                      "message": "You have reached the request rate limit: 15 requests per minute"}}}))
        self._add("array-request-capacity", "other", upstream_error_message=None,
                  upstream_errors=[{"request": {"error": {"message": MESSAGES[0]}}}])
        self._add("non-array-capacity", "other", upstream_error_message=None,
                  upstream_errors={"message": MESSAGES[0]})
        self._add("gateway-nonstream-summary", "other", legacy=False, stream=False,
                  error_owner="platform", error_phase="internal", error_source="gateway",
                  upstream_error_message=None, error_message=MESSAGES[0])
        self._add("null-metadata", "other", legacy=False, account_id=None, group_id=None,
                  error_owner=None, error_phase=None, error_source=None, stream=None,
                  upstream_error_message=None)

        self._add("local-direct", None, legacy=False, status_code=429, upstream_status_code=None,
                  upstream_error_message=None, error_message="您已达到请求数限制：1分钟内最多请求 15 次")
        self._add("local-body", None, legacy=False, status_code=429, upstream_status_code=None,
                  upstream_error_message=None, error_body=json.dumps({"error": {
                      "message": "You have reached the request rate limit: 15 requests per minute"}}))
        self._add("local-array", None, legacy=False, status_code=429, upstream_status_code=None,
                  upstream_error_message=None, upstream_errors=[{
                      "message": "您已达到请求数限制：每分钟最多请求 15 次"}])
        self._add("local-concurrency", None, legacy=False, status_code=429, upstream_status_code=None,
                  error_owner="client", error_phase="concurrency", upstream_error_message=None,
                  error_message="Too many concurrent requests for user")
        self._add("local-concurrency-array", None, legacy=False, status_code=429, upstream_status_code=None,
                  error_owner="platform", error_phase="admission", upstream_error_message=None,
                  upstream_errors=[{"message": "User concurrency limit exceeded"}])

    def _ids(self, category):
        query = "SELECT e.id FROM ops_error_logs e WHERE " + error_category_sql(category) + " ORDER BY e.id"
        return {row["id"] for row in self.connection.execute(query)}

    def test_categories_are_disjoint_and_complete_except_explicit_local_limits(self):
        degradation, other = self._ids("degradation"), self._ids("other")
        self.assertEqual(degradation, self.expected["degradation"])
        self.assertEqual(other, self.expected["other"])
        self.assertFalse(degradation & other)
        self.assertEqual(degradation | other, set(self.rows) - self.excluded)
        for name in ("real-upstream-429", "other-stream", "upstream-concurrency"):
            self.assertIn(self.names[name], other)
        for name in ("array-capacity", "mixed-array-capacity", "direct-stream-capacity", "upstream-capacity-429"):
            self.assertIn(self.names[name], degradation)

    def test_default_category_executes_the_legacy_predicate(self):
        self.assertEqual(error_category_sql(None), ERROR_WHERE)
        actual = self._ids(None)
        self.assertEqual(actual, self.expected[None])
        self.assertIn(self.names["account-auth"], actual)
        self.assertNotIn(self.names["orphan"], actual)

    def test_list_pagination_filters_before_limit_and_preserves_account_filter(self):
        for category in ("degradation", "other", None):
            for aid in (None, 1, 5, 99):
                with self.subTest(category=category, account_id=aid):
                    expected = sorted((eid for eid in self.expected[category]
                                       if aid is None or self.rows[eid]["account_id"] == aid), reverse=True)
                    seen, cursor = [], None
                    for _ in range(len(self.rows) + 1):
                        page = self.service.errors(aid, cursor, 3, category=category)
                        ids = [row["id"] for row in page["items"]]
                        self.assertEqual(ids, expected[len(seen):len(seen) + 3])
                        self.assertFalse(set(ids) & set(seen))
                        seen.extend(ids)
                        if page["next_cursor"] is None:
                            break
                        self.assertEqual(page["next_cursor"], ids[-1])
                        cursor = page["next_cursor"]
                    else:
                        self.fail("pagination did not terminate")
                    self.assertEqual(seen, expected)

    def test_detail_uses_same_category_and_preserves_missing_or_deleted_entities(self):
        for category in ("degradation", "other", None):
            for eid in self.rows:
                with self.subTest(category=category, error_id=eid):
                    if eid in self.expected[category]:
                        detail = self.service.error_detail(eid, category=category)
                        self.assertEqual(detail["id"], eid)
                    else:
                        with self.assertRaises(HTTPException) as caught:
                            self.service.error_detail(eid, category=category)
                        self.assertEqual(caught.exception.status_code, 404)
        orphan = self.service.error_detail(self.names["orphan"], category="other")
        self.assertIsNone(orphan["account_id"])
        missing = self.service.error_detail(self.names["missing-entities"], category="other")
        self.assertEqual(missing["account_id"], 99)
        self.assertIsNone(missing["account_name"])
        self.assertIsNone(missing["group_name"])
        deleted = self.service.error_detail(self.names["soft-deleted-other"], category="other")
        self.assertEqual(deleted["account_name"], "fixture-deleted")
        self.assertEqual(deleted["group_name"], "fixture-deleted-group")
        supported = self.service.error_detail(self.names["soft-deleted-capacity"], category="degradation")
        self.assertEqual(supported["account_id"], 5)


if __name__ == "__main__":
    unittest.main()
