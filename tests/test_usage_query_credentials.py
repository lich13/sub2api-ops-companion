import unittest
from typing import Any

from app import account_ops
from app import sql as sql_module


class UsageQueryCredentialTests(unittest.TestCase):
    def test_quality_sql_does_not_project_account_credentials(self) -> None:
        for query in (
            sql_module.QUALITY_SQL,
            sql_module.QUALITY_ALL_ACCOUNTS_SQL,
            sql_module.GUARD_QUEUE_SQL,
        ):
            self.assertNotIn("a.credentials", query)
            self.assertNotIn("a.extra", query)

    def test_fallback_account_projects_credentials_for_usage_query_hydration(self) -> None:
        class FakeDB:
            sql = ""
            params: dict[str, Any] | None = None

            def fetch_one(self, query: str, params: dict[str, Any] | None = None) -> None:
                self.sql = query
                self.params = params
                return None

        db = FakeDB()
        account_ops.fallback_account(db, 5, load_factor_supported=True)  # type: ignore[arg-type]

        self.assertIn("credentials", db.sql)
        self.assertIn("extra", db.sql)
        self.assertEqual(db.params, {"account_id": 5})


if __name__ == "__main__":
    unittest.main()
