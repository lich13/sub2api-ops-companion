from __future__ import annotations

import copy
import json
import unittest
from types import SimpleNamespace
from unittest.mock import Mock

import httpx
from fastapi import FastAPI, HTTPException

from app import error_evidence
from app.desktop_api import DesktopService, install_desktop_api


def event(**changes):
    return {
        "id": 91,
        "account_id": 31,
        "account_platform": "openai",
        "account_type": "oauth",
        "error_owner": "provider",
        "error_phase": "upstream",
        "error_source": "upstream",
        "stream": True,
        "upstream_status_code": 503,
        "upstream_error_message": error_evidence.MESSAGES[0],
        **changes,
    }


class EventEvidence030Tests(unittest.TestCase):
    def test_only_complete_capacity_messages_on_supported_upstream_accounts_match(self):
        for account_type in ("oauth", "apikey"):
            for message in error_evidence.MESSAGES:
                for changes in (
                    {"upstream_error_message": message},
                    {"upstream_error_message": None, "error_body": json.dumps({"error": {"message": message}})},
                    {"upstream_error_message": None, "upstream_errors": [{"error": {"message": message}}]},
                ):
                    with self.subTest(account_type=account_type, message=message, source=changes):
                        row = event(account_type=account_type, **changes)
                        self.assertEqual(error_evidence.match_message(row), message)
                        self.assertFalse(error_evidence.is_local_throttle(row))

    def test_gateway_wrapped_capacity_keeps_structured_upstream_evidence(self):
        for account_type in ("oauth", "apikey"):
            for message in error_evidence.MESSAGES:
                for event_type in ("response.failed", "response.incomplete"):
                    with self.subTest(account_type=account_type, message=message, event_type=event_type):
                        row = event(
                            account_type=account_type,
                            error_owner="platform",
                            error_phase="internal",
                            error_source="gateway",
                            stream=False,
                            upstream_error_message=None,
                            error_body=json.dumps({"type": event_type, "response": {"error": {"message": message}}}),
                        )
                        self.assertEqual(error_evidence.match_message(row), message)

    def test_other_platforms_local_errors_and_partial_phrases_are_not_degradation(self):
        for changes in (
            {"account_platform": "grok"},
            {"account_type": "other"},
            {"account_platform": None, "account_type": None, "account_id": None},
            {"error_owner": "client", "error_phase": "request"},
            {"error_phase": "account_auth"},
            {"error_owner": "platform", "error_phase": "internal", "error_source": "client"},
            {"upstream_error_message": "Selected model is at capacity."},
            {"upstream_error_message": "Rate limit exceeded", "upstream_status_code": 429},
            {"upstream_error_message": "An error occurred", "upstream_status_code": 500},
        ):
            with self.subTest(changes=changes):
                self.assertIsNone(error_evidence.match_message(event(**changes)))

    def test_deleted_account_history_keeps_known_upstream_identity(self):
        row = event(account_deleted_at="2026-10-01T00:00:00+00:00", account_name=None, group_name=None)
        self.assertEqual(error_evidence.match_message(row), error_evidence.MESSAGES[0])

    def test_request_payload_cannot_supply_capacity_or_local_limiter_evidence(self):
        for message in (error_evidence.MESSAGES[0], "您已达到请求数限制：每分钟最多请求 15 次"):
            row = event(
                upstream_error_message=None,
                error_body=json.dumps({"request": {"error": {"message": message}}}),
                request_body={"message": message},
                prompt=message,
            )
            self.assertIsNone(error_evidence.match_message(row))
            self.assertFalse(error_evidence.is_local_throttle(row))

    def test_explicit_local_request_limit_is_recognized_in_recorded_fields(self):
        for message in (
            "您已达到请求数限制：1分钟内最多请求 15 次",
            "You have reached the request rate limit: 15 requests per minute",
        ):
            for changes in (
                {"error_message": message},
                {"error_body": json.dumps({"error": {"message": message}})},
                {"upstream_errors": [{"message": message}]},
            ):
                with self.subTest(message=message, source=changes):
                    self.assertTrue(error_evidence.is_local_throttle(changes))

    def test_explicit_client_concurrency_limit_is_local(self):
        for message in ("Too many concurrent requests for user", "User concurrency limit exceeded"):
            for source in ({"error_message": message}, {"upstream_errors": [{"message": message}]}):
                with self.subTest(message=message, source=source):
                    row = event(
                        error_owner="client", error_phase="concurrency", upstream_error_message=None,
                        status_code=429, upstream_status_code=None, **source,
                    )
                    self.assertTrue(error_evidence.is_local_throttle(row))
                    self.assertIsNone(error_evidence.match_message(row))

    def test_upstream_429_and_all_capacity_messages_are_not_local_concurrency_limits(self):
        for message in ("Rate limit exceeded", *error_evidence.MESSAGES):
            with self.subTest(message=message):
                row = event(upstream_status_code=429, upstream_error_message=message)
                self.assertFalse(error_evidence.is_local_throttle(row))

    def test_local_event_does_not_hide_independent_capacity_evidence(self):
        row = event(upstream_error_message=None, upstream_errors=[
            {"message": "您已达到请求数限制：每分钟最多请求 15 次"},
            {"message": error_evidence.MESSAGES[2]},
        ])
        original = copy.deepcopy(row)
        self.assertEqual(error_evidence.match_message(row), error_evidence.MESSAGES[2])
        self.assertEqual(row, original)


class EventQuery030Tests(unittest.TestCase):
    def test_default_category_preserves_legacy_predicate_and_rejects_unknown_categories(self):
        self.assertEqual(error_evidence.error_category_sql(None), error_evidence.ERROR_WHERE)
        for value in ("", "all", "legacy", "Degradation", "other OR true"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                error_evidence.error_category_sql(value)

    def test_list_and_detail_use_the_same_category_before_pagination(self):
        for category in (None, "degradation", "other"):
            with self.subTest(category=category):
                db = Mock()
                db.fetch_all.return_value = [{"id": 91}, {"id": 90}, {"id": 89}]
                db.fetch_one.return_value = {"id": 91}
                service = DesktopService(SimpleNamespace(db=db))
                listing = service.errors(31, 92, 2, category=category)
                detail = service.error_detail(91, category=category)
                self.assertEqual([row["id"] for row in listing["items"]], [91, 90])
                self.assertEqual(listing["next_cursor"], 90)
                self.assertEqual(detail["id"], 91)
                list_sql, list_params = db.fetch_all.call_args.args
                detail_sql, detail_params = db.fetch_one.call_args.args
                predicate = error_evidence.error_category_sql(category)
                self.assertIn(predicate, list_sql)
                self.assertIn(predicate, detail_sql)
                self.assertEqual(list_params, {"account_id": 31, "before_id": 92, "limit": 3})
                self.assertEqual(detail_params, {"id": 91})

    def test_other_events_preserve_orphans_and_deleted_entity_history(self):
        db = Mock()
        db.fetch_all.return_value = [
            {"id": 91, "account_id": None, "group_id": None, "account_name": None, "group_name": None},
            {"id": 90, "account_id": 31, "group_id": 41, "account_name": None, "group_name": None},
        ]
        service = DesktopService(SimpleNamespace(db=db))
        result = service.errors(None, None, 2, category="other")
        self.assertEqual([row["id"] for row in result["items"]], [91, 90])
        self.assertIsNone(result["items"][0]["account_id"])
        self.assertEqual(result["items"][1]["account_id"], 31)
        self.assertIsNone(result["next_cursor"])
        sql = db.fetch_all.call_args.args[0].lower()
        self.assertIn("left join accounts", sql)
        self.assertIn("left join groups", sql)
        self.assertNotIn("a.deleted_at", sql)
        self.assertNotIn("g.deleted_at", sql)

    def test_detail_outside_selected_category_is_not_returned(self):
        db = Mock()
        db.fetch_one.return_value = None
        service = DesktopService(SimpleNamespace(db=db))
        for category in ("degradation", "other"):
            with self.subTest(category=category):
                with self.assertRaises(HTTPException) as caught:
                    service.error_detail(91, category=category)
                self.assertEqual(caught.exception.status_code, 404)
                self.assertIn(error_evidence.error_category_sql(category), db.fetch_one.call_args.args[0])


class EventRoutes030Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        app = FastAPI()
        self.service = install_desktop_api(app, SimpleNamespace(db=Mock()))
        self.service.authenticate = Mock()
        self.calls = []

        def listing(account_id, before_id, limit=50, category=None):
            self.calls.append(("list", account_id, before_id, limit, category))
            return {"items": [{"id": 91}], "next_cursor": None}

        def detail(error_id, category=None):
            self.calls.append(("detail", error_id, category))
            return {"id": error_id}

        self.service.errors = listing
        self.service.error_detail = detail
        self.client = httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://127.0.0.1")
        self.addAsyncCleanup(self.client.aclose)

    async def test_optional_category_reaches_both_service_entrypoints(self):
        for category in (None, "degradation", "other"):
            with self.subTest(category=category):
                params = {"account_id": 31, "before_id": 92, "limit": 2}
                if category is not None:
                    params["category"] = category
                listed = await self.client.get("/api/desktop/v1/errors", params=params)
                detail = await self.client.get("/api/desktop/v1/errors/91", params={} if category is None else {"category": category})
                self.assertEqual(listed.status_code, 200, listed.text)
                self.assertEqual(detail.status_code, 200, detail.text)
                self.assertEqual(self.calls[-2:], [("list", 31, 92, 2, category), ("detail", 91, category)])

    async def test_unknown_category_is_rejected_before_any_query(self):
        for category in ("", "all", "legacy", "Degradation", "other OR true"):
            for path in ("/api/desktop/v1/errors", "/api/desktop/v1/errors/91"):
                with self.subTest(category=category, path=path):
                    response = await self.client.get(path, params={"category": category})
                    self.assertEqual(response.status_code, 422, response.text)
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
