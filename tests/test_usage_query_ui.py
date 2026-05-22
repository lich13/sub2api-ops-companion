from __future__ import annotations

import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


class UsageQueryUITests(unittest.TestCase):
    def test_speed_template_exposes_quota_column_and_config_form(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")

        self.assertIn('data-column="quota"', template)
        self.assertIn('<th data-col="quota">额度</th>', template)
        self.assertNotIn('data-column="state"', template)
        self.assertNotIn('data-column="priority"', template)
        self.assertNotIn('<th data-col="state">状态</th>', template)
        self.assertNotIn('<th data-col="priority">优先级</th>', template)
        self.assertIn('colspan="8"', template)
        self.assertIn('action="{{ base_path }}/usage-query/accounts/{{ row.id }}"', template)
        self.assertIn('name="template_type"', template)
        self.assertNotIn('name="use_account_credentials"', template)
        self.assertIn('formaction="{{ base_path }}/usage-query/accounts/{{ row.id }}/fill-credentials"', template)
        self.assertIn('name="upstream_multiplier"', template)
        self.assertIn('name="guard_disable_on_zero"', template)
        self.assertIn('formaction="{{ base_path }}/usage-query/accounts/{{ row.id }}/query"', template)
        self.assertIn('action="{{ base_path }}/usage-query/settings"', template)
        self.assertIn('name="auto_query_interval_minutes"', template)

    def test_usage_query_styles_are_scoped_to_speed_quota_ui(self) -> None:
        style = (REPO_ROOT / "app" / "static" / "style.css").read_text(encoding="utf-8")

        self.assertIn(".quota-cell", style)
        self.assertIn(".usage-query-config", style)
        self.assertIn(".usage-query-template-select", style)
        self.assertIn("width: min(1280px, 100%);", style)
        self.assertNotIn("padding: 0 14px 14px 254px;", style)


if __name__ == "__main__":
    unittest.main()
