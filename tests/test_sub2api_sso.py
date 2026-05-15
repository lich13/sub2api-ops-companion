from __future__ import annotations

import json
import unittest
from io import BytesIO
from typing import Any
from urllib.error import HTTPError

from app.sub2api_sso import Sub2APISSOError, validate_sub2api_token


class FakeHTTPResponse:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.payload = payload

    def __enter__(self) -> "FakeHTTPResponse":
        return self

    def __exit__(self, *_: object) -> None:
        return None

    def read(self) -> bytes:
        return json.dumps(self.payload).encode("utf-8")


class Sub2APISSOTests(unittest.TestCase):
    def test_validates_admin_token_against_sub2api_auth_me(self) -> None:
        calls: list[tuple[Any, dict[str, Any]]] = []

        def opener(request: Any, **kwargs: Any) -> FakeHTTPResponse:
            calls.append((request, kwargs))
            return FakeHTTPResponse(
                {
                    "code": 0,
                    "data": {
                        "id": 7,
                        "username": "admin",
                        "email": "admin@example.com",
                        "role": "admin",
                    },
                }
            )

        user = validate_sub2api_token(
            "https://sub2api.example.com",
            token="jwt-token",
            expected_user_id="7",
            required_role="admin",
            timeout_seconds=3,
            opener=opener,
        )

        self.assertEqual(user.id, 7)
        self.assertEqual(user.username, "admin")
        self.assertEqual(user.role, "admin")
        self.assertEqual(calls[0][0].full_url, "https://sub2api.example.com/api/v1/auth/me")
        self.assertEqual(calls[0][0].headers["Authorization"], "Bearer jwt-token")
        self.assertEqual(calls[0][1]["timeout"], 3)

    def test_rejects_non_admin_role(self) -> None:
        def opener(_request: Any, **_kwargs: Any) -> FakeHTTPResponse:
            return FakeHTTPResponse({"code": 0, "data": {"id": 8, "username": "user", "role": "user"}})

        with self.assertRaises(Sub2APISSOError) as ctx:
            validate_sub2api_token(
                "https://sub2api.example.com",
                token="jwt-token",
                required_role="admin",
                opener=opener,
            )

        self.assertEqual(ctx.exception.reason, "forbidden_role")

    def test_rejects_user_id_mismatch(self) -> None:
        def opener(_request: Any, **_kwargs: Any) -> FakeHTTPResponse:
            return FakeHTTPResponse({"code": 0, "data": {"id": 9, "username": "admin", "role": "admin"}})

        with self.assertRaises(Sub2APISSOError) as ctx:
            validate_sub2api_token(
                "https://sub2api.example.com",
                token="jwt-token",
                expected_user_id="7",
                required_role="admin",
                opener=opener,
            )

        self.assertEqual(ctx.exception.reason, "user_mismatch")

    def test_rejects_invalid_token_response(self) -> None:
        def opener(request: Any, **kwargs: Any) -> FakeHTTPResponse:
            raise HTTPError(request.full_url, 401, "Unauthorized", hdrs=None, fp=BytesIO(b""))

        with self.assertRaises(Sub2APISSOError) as ctx:
            validate_sub2api_token(
                "https://sub2api.example.com",
                token="jwt-token",
                required_role="admin",
                opener=opener,
            )

        self.assertEqual(ctx.exception.reason, "upstream_unauthorized")
        if ctx.exception.__cause__ is not None and hasattr(ctx.exception.__cause__, "close"):
            ctx.exception.__cause__.close()


if __name__ == "__main__":
    unittest.main()
