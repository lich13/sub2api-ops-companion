from __future__ import annotations

import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


class UsageQueryUITests(unittest.TestCase):
    def test_speed_template_exposes_quota_column_and_config_form(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")

        self.assertIn('data-column="quota"', template)
        self.assertIn('<th data-col="quota">额度</th>', template)
        self.assertIn('action="{{ base_path }}/usage-query/accounts/{{ row.id }}"', template)
        self.assertIn('name="template_type"', template)
        self.assertIn('name="upstream_multiplier"', template)
        self.assertIn('name="guard_disable_on_zero"', template)
        self.assertIn('formaction="{{ base_path }}/usage-query/accounts/{{ row.id }}/query"', template)

    def test_usage_query_styles_are_scoped_to_speed_quota_ui(self) -> None:
        style = (REPO_ROOT / "app" / "static" / "style.css").read_text(encoding="utf-8")

        self.assertIn(".quota-cell", style)
        self.assertIn(".usage-query-config", style)
        self.assertIn(".usage-query-template-select", style)


if __name__ == "__main__":
    unittest.main()
