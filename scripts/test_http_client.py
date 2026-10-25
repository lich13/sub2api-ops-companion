from __future__ import annotations

from io import BytesIO
import json
import unittest
from unittest.mock import Mock

from server_http_contract import CheckFailure, HttpClient


def client_for(raw: bytes, *, limit: int = 4_000_000) -> HttpClient:
    response = BytesIO(raw)
    response.status = 200
    response.headers = {"Content-Type": "application/json"}
    client = HttpClient("http://127.0.0.1", max_response_bytes=limit)
    client.opener = Mock()
    client.opener.open.return_value = response
    return client


class HttpClientTests(unittest.TestCase):
    def test_large_snapshot_is_read_completely_with_benchmark_limit(self) -> None:
        raw = json.dumps({"accounts": [{"id": 1, "fixture": "x" * 4_000_000}]}).encode()
        with self.assertRaisesRegex(CheckFailure, "HTTP response exceeds fixture read limit"):
            client_for(raw).request("GET", "/snapshot")

        response = client_for(raw, limit=8_000_000).request("GET", "/snapshot")

        self.assertEqual(response.status, 200)
        self.assertEqual(response.body["accounts"][0]["id"], 1)
        self.assertEqual(len(response.body["accounts"][0]["fixture"]), 4_000_000)

    def test_exact_limit_is_accepted_and_excess_is_explicit(self) -> None:
        raw = b'{"accounts":[]}'
        self.assertEqual(client_for(raw, limit=len(raw)).request("GET", "/snapshot").body, {"accounts": []})
        with self.assertRaisesRegex(CheckFailure, "^HTTP response exceeds fixture read limit$"):
            client_for(raw, limit=len(raw) - 1).request("GET", "/snapshot")

    def test_invalid_json_within_limit_keeps_non_json_diagnostic(self) -> None:
        raw = b"fixture invalid json"
        self.assertEqual(client_for(raw).request("GET", "/snapshot").body,
                         {"non_json_response": True, "bytes": len(raw)})


if __name__ == "__main__":
    unittest.main()
