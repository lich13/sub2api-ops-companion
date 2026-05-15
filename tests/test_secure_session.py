from __future__ import annotations

import unittest
import tempfile
from pathlib import Path

from app.secure_session import create_session_cookie, read_session_cookie


class SecureSessionTests(unittest.TestCase):
    def test_opaque_session_round_trips_without_exposing_username(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            store_path = str(Path(tmpdir) / "sessions.json")
            cookie = create_session_cookie(
                "sub2api:7:admin",
                secret="very-long-session-secret",
                store_path=store_path,
                issued_at=1_800_000_000,
                ttl_seconds=3600,
                source="sub2api_sso",
            )

            self.assertTrue(cookie.startswith("v2."))
            self.assertNotIn("sub2api", cookie)
            self.assertNotIn("admin", cookie)

            payload = read_session_cookie(
                cookie,
                secret="very-long-session-secret",
                store_path=store_path,
                max_age_seconds=3600,
                now=1_800_000_100,
            )

            self.assertIsNotNone(payload)
            self.assertEqual(payload.username, "sub2api:7:admin")
            self.assertEqual(payload.source, "sub2api_sso")

    def test_opaque_session_rejects_expired_payload(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            store_path = str(Path(tmpdir) / "sessions.json")
            cookie = create_session_cookie(
                "admin",
                secret="very-long-session-secret",
                store_path=store_path,
                issued_at=1_800_000_000,
                ttl_seconds=60,
            )

            payload = read_session_cookie(
                cookie,
                secret="very-long-session-secret",
                store_path=store_path,
                max_age_seconds=3600,
                now=1_800_000_061,
            )

            self.assertIsNone(payload)

    def test_opaque_session_rejects_wrong_secret(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            store_path = str(Path(tmpdir) / "sessions.json")
            cookie = create_session_cookie(
                "admin",
                secret="very-long-session-secret",
                store_path=store_path,
                issued_at=1_800_000_000,
                ttl_seconds=3600,
            )

            payload = read_session_cookie(
                cookie,
                secret="different-secret",
                store_path=store_path,
                max_age_seconds=3600,
                now=1_800_000_100,
            )

            self.assertIsNone(payload)


if __name__ == "__main__":
    unittest.main()
